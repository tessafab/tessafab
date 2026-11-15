//! Heatshrink compression (LZSS), as used by Prusa binary G-code.
//!
//! The bit stream of the reference implementation (github.com/atomicobject/heatshrink):
//! most significant bit first; a 1 bit and 8 bits is a literal byte; a 0 bit,
//! `window` bits of (distance - 1) and `lookahead` bits of (length - 1) copy
//! earlier output. The last byte is padded with zero bits, which a decoder
//! ignores because they never make a whole item. Prusa uses window 11 or 12
//! with lookahead 4.

use std::collections::HashMap;

#[derive(Debug, PartialEq, Eq)]
pub struct Error(pub &'static str);

struct BitWriter {
    out: Vec<u8>,
    cur: u8,
    used: u8,
}

impl BitWriter {
    fn put(&mut self, value: u32, bits: u8) {
        for k in (0..bits).rev() {
            self.cur = (self.cur << 1) | ((value >> k) & 1) as u8;
            self.used += 1;
            if self.used == 8 {
                self.out.push(self.cur);
                self.cur = 0;
                self.used = 0;
            }
        }
    }

    fn finish(mut self) -> Vec<u8> {
        if self.used > 0 {
            self.out.push(self.cur << (8 - self.used));
        }
        self.out
    }
}

struct BitReader<'a> {
    data: &'a [u8],
    pos: usize, // in bits
}

impl BitReader<'_> {
    fn take(&mut self, bits: u8) -> Option<u32> {
        if self.pos + bits as usize > self.data.len() * 8 {
            return None;
        }
        let mut v = 0u32;
        for _ in 0..bits {
            let byte = self.data[self.pos / 8];
            let bit = (byte >> (7 - (self.pos % 8))) & 1;
            v = (v << 1) | bit as u32;
            self.pos += 1;
        }
        Some(v)
    }
}

fn check(window: u8, lookahead: u8) -> Result<(), Error> {
    if !(4..=15).contains(&window) || !(3..window).contains(&lookahead) {
        return Err(Error("unsupported heatshrink window or lookahead size"));
    }
    Ok(())
}

/// Decompress `data` made with the given window and lookahead sizes (log2).
pub fn decompress(data: &[u8], window: u8, lookahead: u8) -> Result<Vec<u8>, Error> {
    check(window, lookahead)?;
    let mut r = BitReader { data, pos: 0 };
    let mut out: Vec<u8> = Vec::with_capacity(data.len() * 3);
    while let Some(tag) = r.take(1) {
        if tag == 1 {
            match r.take(8) {
                Some(b) => out.push(b as u8),
                None => break,
            }
        } else {
            let Some(index) = r.take(window) else { break };
            let Some(count) = r.take(lookahead) else {
                break;
            };
            let dist = index as usize + 1;
            let len = count as usize + 1;
            for _ in 0..len {
                // the reference decoder's window starts as zeros
                let b = if dist <= out.len() {
                    out[out.len() - dist]
                } else {
                    0
                };
                out.push(b);
            }
        }
    }
    Ok(out)
}

/// Compress `data` with the given window and lookahead sizes (log2).
/// Any valid stream decodes the same; this one finds the longest match among
/// earlier positions with the same 3 leading bytes, nearest first.
pub fn compress(data: &[u8], window: u8, lookahead: u8) -> Result<Vec<u8>, Error> {
    check(window, lookahead)?;
    let max_dist = 1usize << window;
    let max_len = 1usize << lookahead;
    // a back-reference costs 1 + window + lookahead bits, a literal 9
    let min_len = (1 + window as usize + lookahead as usize) / 9 + 1;
    let mut w = BitWriter {
        out: Vec::with_capacity(data.len() / 2),
        cur: 0,
        used: 0,
    };
    let mut heads: HashMap<[u8; 3], Vec<usize>> = HashMap::new();
    let key = |i: usize| -> Option<[u8; 3]> {
        (i + 3 <= data.len()).then(|| [data[i], data[i + 1], data[i + 2]])
    };
    let mut i = 0;
    while i < data.len() {
        let mut best_len = 0;
        let mut best_dist = 0;
        if let Some(k) = key(i) {
            if let Some(cands) = heads.get(&k) {
                for &j in cands.iter().rev() {
                    let dist = i - j;
                    if dist > max_dist {
                        break;
                    }
                    let mut n = 0;
                    while n < max_len && i + n < data.len() && data[j + n] == data[i + n] {
                        n += 1;
                    }
                    if n > best_len {
                        best_len = n;
                        best_dist = dist;
                        if n == max_len {
                            break;
                        }
                    }
                }
            }
        }
        let step = if best_len >= min_len.max(3) {
            w.put(0, 1);
            w.put((best_dist - 1) as u32, window);
            w.put((best_len - 1) as u32, lookahead);
            best_len
        } else {
            w.put(1, 1);
            w.put(data[i] as u32, 8);
            1
        };
        for p in i..i + step {
            if let Some(k) = key(p) {
                let v = heads.entry(k).or_default();
                v.push(p);
                // keep only positions still inside the window
                if v.len() > 64 && p - v[0] > max_dist {
                    let cut = v.iter().position(|&q| p - q <= max_dist).unwrap_or(v.len());
                    v.drain(..cut);
                }
            }
        }
        i += step;
    }
    Ok(w.finish())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip(data: &[u8], window: u8) {
        let c = compress(data, window, 4).unwrap();
        assert_eq!(decompress(&c, window, 4).unwrap(), data);
    }

    #[test]
    fn round_trips() {
        roundtrip(b"", 12);
        roundtrip(b"a", 12);
        roundtrip(b"G1 X10 Y10 E0.5\nG1 X10 Y10 E0.5\nG1 X10 Y10 E0.5\n", 11);
        let text: Vec<u8> = (0..20_000)
            .map(|k| b"G1 X1.25 Y-3 E.04\n;c\n"[k % 21] ^ ((k / 997) as u8 & 1))
            .collect();
        roundtrip(&text, 11);
        roundtrip(&text, 12);
        let noise: Vec<u8> = (0..5000u32)
            .map(|k| (k.wrapping_mul(2654435761) >> 13) as u8)
            .collect();
        roundtrip(&noise, 12);
    }

    #[test]
    fn compresses_repetitive_text() {
        let text = b"G1 X100.5 Y20.25 E0.0421\n".repeat(400);
        let c = compress(&text, 12, 4).unwrap();
        assert!(c.len() * 4 < text.len(), "{} -> {}", text.len(), c.len());
    }

    #[test]
    fn known_stream() {
        // "aaaa" with window 8 / lookahead 4: literal 'a', then copy 3 from distance 1
        let mut w = BitWriter {
            out: vec![],
            cur: 0,
            used: 0,
        };
        w.put(1, 1);
        w.put(b'a' as u32, 8);
        w.put(0, 1);
        w.put(0, 8);
        w.put(2, 4);
        assert_eq!(decompress(&w.finish(), 8, 4).unwrap(), b"aaaa");
    }
}
