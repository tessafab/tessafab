//! Prusa binary G-code (.bgcode): read the G-code out, write new G-code back in.
//!
//! Format: <https://github.com/prusa3d/libbgcode/blob/main/doc/specifications.md>.
//! A file is a 10-byte header (`GCDE`, version, checksum type) and a list of
//! blocks: header (type, compression, sizes), parameters, data, and a CRC32 of
//! all of those. G-code blocks are usually Heatshrink-compressed and
//! MeatPack-encoded; the MeatPack code follows libbgcode's meatpack.cpp.
//!
//! Writing keeps every non-G-code block byte for byte (metadata, thumbnails)
//! and re-encodes the G-code with the compression and encoding the input used,
//! in blocks no bigger than the input's. A port of bgcode.py.

use crate::heatshrink;
use std::collections::BTreeMap;
use std::io::{Read, Write};

pub const MAGIC: &[u8; 4] = b"GCDE";
pub const GCODE_BLOCK: u16 = 1;
pub const NO_COMPRESSION: u16 = 0;
pub const DEFLATE: u16 = 1;
pub const HEATSHRINK_11_4: u16 = 2;
pub const HEATSHRINK_12_4: u16 = 3;
pub const NO_ENCODING: u16 = 0;
pub const MEATPACK: u16 = 1;
pub const MEATPACK_COMMENTS: u16 = 2;
pub const CHECKSUM_NONE: u16 = 0;
pub const CHECKSUM_CRC32: u16 = 1;

#[derive(Debug, PartialEq, Eq)]
pub struct Error(pub String);

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

fn err<T>(msg: impl Into<String>) -> Result<T, Error> {
    Err(Error(msg.into()))
}

pub fn is_bgcode(data: &[u8]) -> bool {
    data.starts_with(MAGIC)
}

#[derive(Clone, Debug)]
pub struct Block {
    pub kind: u16,
    pub compression: u16,
    /// Uncompressed size of the data.
    pub size: u32,
    /// Raw parameter bytes.
    pub params: Vec<u8>,
    /// The stored (possibly compressed) data.
    pub payload: Vec<u8>,
    /// The whole block as it was in the file.
    pub raw: Vec<u8>,
}

impl Block {
    pub fn encoding(&self) -> u16 {
        u16::from_le_bytes([self.params[0], self.params[1]])
    }
}

fn param_size(kind: u16) -> usize {
    // thumbnails: format, width, height; every other block: encoding
    if kind == 5 {
        6
    } else {
        2
    }
}

// --------------------------------------------------------------------------- //
// Compression
// --------------------------------------------------------------------------- //

pub fn decompress(data: &[u8], compression: u16, size: u32) -> Result<Vec<u8>, Error> {
    let out = match compression {
        NO_COMPRESSION => data.to_vec(),
        DEFLATE => {
            let mut out = Vec::with_capacity(size as usize);
            flate2::read::ZlibDecoder::new(data)
                .read_to_end(&mut out)
                .map_err(|e| Error(format!("can't inflate a block: {e}")))?;
            out
        }
        HEATSHRINK_11_4 | HEATSHRINK_12_4 => {
            let window = if compression == HEATSHRINK_11_4 {
                11
            } else {
                12
            };
            heatshrink::decompress(data, window, 4).map_err(|e| Error(e.0.into()))?
        }
        c => return err(format!("unknown compression {c}")),
    };
    if out.len() != size as usize {
        return err(format!(
            "block decompressed to {} bytes, expected {}",
            out.len(),
            size
        ));
    }
    Ok(out)
}

pub fn compress(data: &[u8], compression: u16) -> Result<Vec<u8>, Error> {
    match compression {
        NO_COMPRESSION => Ok(data.to_vec()),
        DEFLATE => {
            let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
            z.write_all(data).map_err(|e| Error(e.to_string()))?;
            z.finish().map_err(|e| Error(e.to_string()))
        }
        HEATSHRINK_11_4 | HEATSHRINK_12_4 => {
            let window = if compression == HEATSHRINK_11_4 {
                11
            } else {
                12
            };
            heatshrink::compress(data, window, 4).map_err(|e| Error(e.0.into()))
        }
        c => err(format!("unknown compression {c}")),
    }
}

// --------------------------------------------------------------------------- //
// MeatPack (port of libbgcode src/LibBGCode/binarize/meatpack.cpp)
// --------------------------------------------------------------------------- //

const SIGNAL: u8 = 0xFF;
const ENABLE_PACKING: u8 = 251;
const DISABLE_PACKING: u8 = 250;
const RESET_ALL: u8 = 249;
const ENABLE_NO_SPACES: u8 = 247;
const DISABLE_NO_SPACES: u8 = 246;
const CODES: &[u8; 15] = b"0123456789. \nGX";
const G_PARAMS: &[u8] = b"XYZEFIJRSGPWHCA";

struct Unpacker {
    packing: bool,
    nospace: bool,
    full_queue: u32,
    char_buf: u8,
}

impl Unpacker {
    fn char_of(&self, n: u8) -> u8 {
        if n == 0b1011 {
            if self.nospace {
                b'E'
            } else {
                b' '
            }
        } else {
            CODES[n as usize]
        }
    }

    fn rx(&mut self, c: u8, emit: &mut Vec<u8>) {
        if !self.packing {
            emit.push(c);
            return;
        }
        if self.full_queue > 0 {
            emit.push(c);
            if self.char_buf != 0 {
                emit.push(self.char_buf);
                self.char_buf = 0;
            }
            self.full_queue -= 1;
            return;
        }
        let first_full = (c & 0x0F) == 0x0F;
        let second_full = (c & 0xF0) == 0xF0;
        if first_full {
            self.full_queue += 1;
            if second_full {
                self.full_queue += 1;
            } else {
                self.char_buf = self.char_of((c >> 4) & 0xF);
            }
        } else {
            let lo = self.char_of(c & 0xF);
            emit.push(lo);
            if lo != b'\n' {
                if second_full {
                    self.full_queue += 1;
                } else {
                    emit.push(self.char_of((c >> 4) & 0xF));
                }
            }
        }
    }
}

/// Unpack MeatPack bytes to G-code text bytes (spaces put back in G lines).
pub fn meatpack_decode(src: &[u8]) -> Vec<u8> {
    let mut u = Unpacker {
        packing: false,
        nospace: false,
        full_queue: 0,
        char_buf: 0,
    };
    let mut cmd_active = false;
    let mut cmd_count = 0;
    let mut out: Vec<u8> = Vec::with_capacity(src.len() * 2);
    let mut add_space = false;
    let mut emit: Vec<u8> = Vec::with_capacity(4);
    for &c in src {
        emit.clear();
        if c == SIGNAL {
            if cmd_count > 0 {
                cmd_active = true;
                cmd_count = 0;
            } else {
                cmd_count += 1;
            }
        } else if cmd_active {
            match c {
                ENABLE_PACKING => u.packing = true,
                DISABLE_PACKING | RESET_ALL => u.packing = false,
                ENABLE_NO_SPACES => u.nospace = true,
                DISABLE_NO_SPACES => u.nospace = false,
                _ => {}
            }
            cmd_active = false;
        } else {
            if cmd_count > 0 {
                u.rx(SIGNAL, &mut emit);
                cmd_count = 0;
            }
            u.rx(c, &mut emit);
        }
        for &ch in &emit {
            let mut new_line = false;
            if ch == b'G' && (out.is_empty() || *out.last().unwrap() == b'\n') {
                add_space = true;
                new_line = true;
            } else if ch == b'\n' || ch == b' ' {
                add_space = false;
            }
            if !new_line && add_space && G_PARAMS.contains(&ch) {
                out.push(b' ');
            }
            if ch != b'\n' || out.last() != Some(&b'\n') {
                out.push(ch);
            }
        }
    }
    out
}

/// G0-G3 lines lose their spaces (the decoder puts them back).
fn compact_g_line(line: &[u8]) -> Vec<u8> {
    if line.len() < 2 || line[0] != b'G' || !line[1].is_ascii_digit() {
        return line.to_vec();
    }
    let digits: Vec<u8> = line[1..]
        .iter()
        .take_while(|c| c.is_ascii_digit())
        .copied()
        .collect();
    let num: u64 = std::str::from_utf8(&digits)
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(99);
    if num > 3 {
        return line.to_vec();
    }
    let mut result: Vec<u8> = line
        .iter()
        .filter(|&&c| c != b' ')
        .map(|&c| match c {
            b'e' => b'E',
            b'x' => b'X',
            b'g' => b'G',
            c => c,
        })
        .collect();
    if result.contains(&b'*') {
        result.retain(|&c| c != b'*');
        let cs = result.iter().fold(0u8, |a, &c| a ^ c);
        result.extend_from_slice(format!("*{cs}").as_bytes());
    }
    result.push(b'\n');
    result
}

fn split_lines_keep(text: &[u8]) -> impl Iterator<Item = &[u8]> {
    let mut start = 0;
    std::iter::from_fn(move || {
        if start >= text.len() {
            return None;
        }
        let end = text[start..]
            .iter()
            .position(|&c| c == b'\n')
            .map(|p| start + p + 1)
            .unwrap_or(text.len());
        let line = &text[start..end];
        start = end;
        Some(line)
    })
}

/// MeatPack-encode G-code text (no-spaces mode, as PrusaSlicer writes it).
pub fn meatpack_encode(text: &[u8], keep_comments: bool) -> Result<Vec<u8>, Error> {
    // no-spaces mode: 'E' takes the space's code
    let code_of = |c: u8| -> Option<u8> {
        match c {
            b' ' => None,
            b'E' => Some(11),
            c => CODES.iter().position(|&k| k == c).map(|p| p as u8),
        }
    };
    let mut out = vec![
        SIGNAL,
        SIGNAL,
        ENABLE_PACKING,
        SIGNAL,
        SIGNAL,
        ENABLE_NO_SPACES,
    ];
    let mut packing = true;
    for line in split_lines_keep(text) {
        let stripped: &[u8] = {
            let k = line
                .iter()
                .position(|&c| c != b' ' && c != b'\t')
                .unwrap_or(line.len());
            &line[k..]
        };
        if keep_comments && stripped.first() == Some(&b';') {
            if line.contains(&0xFF) {
                return err("a comment contains byte 0xFF, which MeatPack can't store");
            }
            if packing {
                out.extend_from_slice(&[SIGNAL, SIGNAL, DISABLE_PACKING]);
                packing = false;
            }
            out.extend_from_slice(line);
            continue;
        }
        if stripped.is_empty() || matches!(stripped[0], b';' | b'\n' | b'\r') || line.len() < 2 {
            continue;
        }
        let code_part = line.split(|&c| c == b';').next().unwrap_or(&[]);
        let trim = |s: &[u8]| -> Vec<u8> {
            let is_ws = |c: &u8| matches!(c, b' ' | b'\t' | b'\r' | b'\n');
            let a = s.iter().position(|c| !is_ws(c)).unwrap_or(s.len());
            let b = s
                .iter()
                .rposition(|c| !is_ws(c))
                .map(|p| p + 1)
                .unwrap_or(a);
            s[a..b.max(a)].to_vec()
        };
        let code = trim(code_part);
        if code.is_empty() {
            continue;
        }
        let mut code = compact_g_line(&code);
        if code.last() != Some(&b'\n') {
            code.push(b'\n');
        }
        if code.contains(&0xFF) {
            return err("a line contains byte 0xFF, which MeatPack can't store");
        }
        let mut buf = Vec::with_capacity(code.len());
        let n = code.len();
        let mut i = 0;
        while i < n {
            let c1 = code[i];
            let c2 = if i == n - 1 { b'\n' } else { code[i + 1] };
            match (code_of(c1), code_of(c2)) {
                (Some(p1), Some(p2)) => buf.push((p2 << 4) | p1),
                (Some(p1), None) => {
                    buf.push(0xF0 | p1);
                    buf.push(c2);
                }
                (None, Some(p2)) => {
                    buf.push((p2 << 4) | 0x0F);
                    buf.push(c1);
                }
                (None, None) => buf.extend_from_slice(&[0xFF, c1, c2]),
            }
            i += 2;
        }
        if !packing && !buf.is_empty() {
            out.extend_from_slice(&[SIGNAL, SIGNAL, ENABLE_PACKING]);
            packing = true;
        }
        out.extend_from_slice(&buf);
    }
    if !keep_comments {
        out.extend_from_slice(&[SIGNAL, SIGNAL, RESET_ALL]);
    }
    Ok(out)
}

pub fn decode_gcode(data: Vec<u8>, encoding: u16) -> Result<Vec<u8>, Error> {
    match encoding {
        NO_ENCODING => Ok(data),
        MEATPACK | MEATPACK_COMMENTS => Ok(meatpack_decode(&data)),
        e => err(format!("unknown G-code encoding {e}")),
    }
}

pub fn encode_gcode(text: &[u8], encoding: u16) -> Result<Vec<u8>, Error> {
    match encoding {
        NO_ENCODING => Ok(text.to_vec()),
        _ => meatpack_encode(text, encoding == MEATPACK_COMMENTS),
    }
}

// --------------------------------------------------------------------------- //
// File
// --------------------------------------------------------------------------- //

#[derive(Clone, Debug)]
pub struct BgcodeFile {
    pub version: u32,
    pub checksum: u16,
    pub blocks: Vec<Block>,
}

fn u16_at(d: &[u8], p: usize) -> u16 {
    u16::from_le_bytes([d[p], d[p + 1]])
}

fn u32_at(d: &[u8], p: usize) -> u32 {
    u32::from_le_bytes([d[p], d[p + 1], d[p + 2], d[p + 3]])
}

impl BgcodeFile {
    pub fn parse(data: &[u8]) -> Result<Self, Error> {
        if data.len() < 10 || !is_bgcode(data) {
            return err("not a binary G-code file");
        }
        let version = u32_at(data, 4);
        let checksum = u16_at(data, 8);
        if checksum != CHECKSUM_NONE && checksum != CHECKSUM_CRC32 {
            return err(format!("unknown checksum type {checksum}"));
        }
        let mut blocks = Vec::new();
        let mut pos = 10;
        while pos < data.len() {
            let start = pos;
            let short = || err(format!("file is cut short in a block at byte {start}"));
            if pos + 8 > data.len() {
                return short();
            }
            let kind = u16_at(data, pos);
            let compression = u16_at(data, pos + 2);
            let size = u32_at(data, pos + 4);
            pos += 8;
            let csize = if compression != NO_COMPRESSION {
                if pos + 4 > data.len() {
                    return short();
                }
                let c = u32_at(data, pos);
                pos += 4;
                c
            } else {
                size
            } as usize;
            let np = param_size(kind);
            if pos + np + csize > data.len() {
                return short();
            }
            let params = data[pos..pos + np].to_vec();
            pos += np;
            let payload = data[pos..pos + csize].to_vec();
            pos += csize;
            if checksum == CHECKSUM_CRC32 {
                if pos + 4 > data.len() {
                    return short();
                }
                if crc32fast::hash(&data[start..pos]) != u32_at(data, pos) {
                    return err(format!("block at byte {start} fails its checksum"));
                }
                pos += 4;
            }
            blocks.push(Block {
                kind,
                compression,
                size,
                params,
                payload,
                raw: data[start..pos].to_vec(),
            });
        }
        Ok(BgcodeFile {
            version,
            checksum,
            blocks,
        })
    }

    pub fn gcode_blocks(&self) -> impl Iterator<Item = &Block> {
        self.blocks.iter().filter(|b| b.kind == GCODE_BLOCK)
    }

    pub fn block_text(&self, b: &Block) -> Result<Vec<u8>, Error> {
        decode_gcode(decompress(&b.payload, b.compression, b.size)?, b.encoding())
    }

    /// All the G-code, as bytes (Latin-1 text).
    pub fn gcode(&self) -> Result<Vec<u8>, Error> {
        let mut out = Vec::new();
        for b in self.gcode_blocks() {
            out.extend(self.block_text(b)?);
        }
        Ok(out)
    }

    /// {key: value} of an INI metadata block (2 slicer, 3 printer, 4 print, 0 file).
    pub fn metadata(&self, kind: u16) -> Result<BTreeMap<String, String>, Error> {
        let mut out = BTreeMap::new();
        for b in self.blocks.iter().filter(|b| b.kind == kind) {
            let text = decompress(&b.payload, b.compression, b.size)?;
            for line in String::from_utf8_lossy(&text).lines() {
                if let Some((k, v)) = line.split_once('=') {
                    out.insert(k.trim().to_string(), v.trim().to_string());
                }
            }
        }
        Ok(out)
    }

    /// The metadata block's entries in file order (for "; key = value" lines).
    pub fn metadata_lines(&self, kind: u16) -> Result<Vec<(String, String)>, Error> {
        let mut out: Vec<(String, String)> = Vec::new();
        for b in self.blocks.iter().filter(|b| b.kind == kind) {
            let text = decompress(&b.payload, b.compression, b.size)?;
            for line in String::from_utf8_lossy(&text).lines() {
                if let Some((k, v)) = line.split_once('=') {
                    let (k, v) = (k.trim().to_string(), v.trim().to_string());
                    match out.iter_mut().find(|(kk, _)| *kk == k) {
                        Some(e) => e.1 = v,
                        None => out.push((k, v)),
                    }
                }
            }
        }
        Ok(out)
    }

    /// This file with its G-code replaced by `gcode` (Latin-1 text bytes).
    pub fn with_gcode(&self, gcode: &[u8]) -> Result<Vec<u8>, Error> {
        let first = match self.gcode_blocks().next() {
            Some(b) => b,
            None => return err("the original file has no G-code blocks"),
        };
        let limit = self
            .gcode_blocks()
            .map(|b| b.size as usize)
            .max()
            .unwrap_or(0)
            .max(1024);
        // split on line boundaries into chunks no bigger than the input's blocks
        let mut chunks: Vec<&[u8]> = Vec::new();
        let (mut start, mut len) = (0usize, 0usize);
        let mut pos = 0;
        for line in split_lines_keep(gcode) {
            if len > 0 && len + line.len() > limit {
                chunks.push(&gcode[start..pos]);
                start = pos;
                len = 0;
            }
            len += line.len();
            pos += line.len();
        }
        if len > 0 {
            chunks.push(&gcode[start..pos]);
        }
        let mut out = Vec::with_capacity(gcode.len() / 2);
        out.extend_from_slice(MAGIC);
        out.extend_from_slice(&self.version.to_le_bytes());
        out.extend_from_slice(&self.checksum.to_le_bytes());
        let mut wrote = false;
        for b in &self.blocks {
            if b.kind != GCODE_BLOCK {
                out.extend_from_slice(&b.raw);
                continue;
            }
            if wrote {
                continue;
            }
            for chunk in &chunks {
                let data = encode_gcode(chunk, first.encoding())?;
                out.extend(block_bytes(
                    GCODE_BLOCK,
                    first.compression,
                    &first.params,
                    &data,
                    self.checksum,
                )?);
            }
            wrote = true;
        }
        Ok(out)
    }
}

fn block_bytes(
    kind: u16,
    compression: u16,
    params: &[u8],
    data: &[u8],
    checksum: u16,
) -> Result<Vec<u8>, Error> {
    let payload = compress(data, compression)?;
    let mut body = Vec::with_capacity(payload.len() + 20);
    body.extend_from_slice(&kind.to_le_bytes());
    body.extend_from_slice(&compression.to_le_bytes());
    body.extend_from_slice(&(data.len() as u32).to_le_bytes());
    if compression != NO_COMPRESSION {
        body.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    }
    body.extend_from_slice(params);
    body.extend_from_slice(&payload);
    if checksum == CHECKSUM_CRC32 {
        let crc = crc32fast::hash(&body);
        body.extend_from_slice(&crc.to_le_bytes());
    }
    Ok(body)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn meatpack_round_trips() {
        let text = "; a comment line\nG1 X10.5 Y-3.25 E0.0421 F1200\nG0 Z0.6\nM104 S215 ; trailing comment\n\
                    G28 W\nM862.3 P \"MINI\"\nG2 X1 Y2 I3 J4\nT0\n";
        for keep in [true, false] {
            let back = meatpack_decode(&meatpack_encode(text.as_bytes(), keep).unwrap());
            let want: Vec<String> = text
                .lines()
                .filter(|l| keep || !l.starts_with(';'))
                .map(|l| {
                    if l.starts_with(';') {
                        l.to_string()
                    } else {
                        l.split(';').next().unwrap().trim().to_string()
                    }
                })
                .collect();
            let got: Vec<String> = String::from_utf8_lossy(&back)
                .lines()
                .map(String::from)
                .collect();
            assert_eq!(got, want, "keep_comments={keep}");
        }
    }

    #[test]
    fn meatpack_refuses_0xff() {
        assert!(meatpack_encode(b"M117 \xff\n", true).is_err());
    }
}
