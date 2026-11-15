//! Reading SVG drawings into shapes in mm: patterns, layouts and composed
//! drawings. A port of the SVG half of tilepattern.py (`read_svg` and helpers).
//!
//! Coordinates come out in mm with y pointing up (the SVG's bottom-left corner
//! is (0, 0)), so a drawing reads the same way as the bed seen from above.
//! Curves are flattened to within `tol_mm` (0.15 mm by default) exactly as the
//! Python engine does, so both see the same shapes.

use std::collections::HashMap;
use std::f64::consts::PI;
use std::fmt;

#[derive(Debug, Clone, PartialEq)]
pub struct Error(pub String);

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Error {}

fn err<T>(msg: impl Into<String>) -> Result<T, Error> {
    Err(Error(msg.into()))
}

pub type Pt = (f64, f64);
/// One sub-path: its points and whether it is closed.
pub type Subpath = (Vec<Pt>, bool);

// --------------------------------------------------------------------------- //
// Colours
// --------------------------------------------------------------------------- //

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Paint {
    Rgb(u8, u8, u8),
    /// A gradient or pattern fill (`url(#...)`).
    Url,
}

const NAMED: [(&str, (u8, u8, u8)); 37] = [
    ("black", (0, 0, 0)),
    ("white", (255, 255, 255)),
    ("red", (255, 0, 0)),
    ("lime", (0, 255, 0)),
    ("green", (0, 128, 0)),
    ("blue", (0, 0, 255)),
    ("yellow", (255, 255, 0)),
    ("cyan", (0, 255, 255)),
    ("aqua", (0, 255, 255)),
    ("magenta", (255, 0, 255)),
    ("fuchsia", (255, 0, 255)),
    ("gray", (128, 128, 128)),
    ("grey", (128, 128, 128)),
    ("silver", (192, 192, 192)),
    ("maroon", (128, 0, 0)),
    ("navy", (0, 0, 128)),
    ("olive", (128, 128, 0)),
    ("purple", (128, 0, 128)),
    ("teal", (0, 128, 128)),
    ("orange", (255, 165, 0)),
    ("pink", (255, 192, 203)),
    ("lightcoral", (240, 128, 128)),
    ("salmon", (250, 128, 114)),
    ("lightsalmon", (255, 160, 122)),
    ("tomato", (255, 99, 71)),
    ("indianred", (205, 92, 92)),
    ("lightpink", (255, 182, 193)),
    ("royalblue", (65, 105, 225)),
    ("mediumblue", (0, 0, 205)),
    ("darkblue", (0, 0, 139)),
    ("brown", (165, 42, 42)),
    ("tan", (210, 180, 140)),
    ("beige", (245, 245, 220)),
    ("lightgray", (211, 211, 211)),
    ("lightgrey", (211, 211, 211)),
    ("darkgray", (169, 169, 169)),
    ("darkgrey", (169, 169, 169)),
];

/// A colour value: `Some(Paint)`, or `None` for none / transparent / unreadable.
pub fn parse_color(v: Option<&str>) -> Option<Paint> {
    let v = v?.trim().to_ascii_lowercase();
    if v.is_empty() || v == "none" || v == "transparent" {
        return None;
    }
    if v.starts_with("url(") {
        return Some(Paint::Url);
    }
    if let Some(h) = v.strip_prefix('#') {
        let hex = |s: &str| u8::from_str_radix(s, 16).ok();
        return match h.len() {
            3 | 4 => {
                let c: Vec<char> = h.chars().collect();
                let d = |k: usize| hex(&format!("{}{}", c[k], c[k]));
                Some(Paint::Rgb(d(0)?, d(1)?, d(2)?))
            }
            6 | 8 => Some(Paint::Rgb(
                hex(h.get(0..2)?)?,
                hex(h.get(2..4)?)?,
                hex(h.get(4..6)?)?,
            )),
            _ => None,
        };
    }
    if v.starts_with("rgb") {
        let inner = v.split_once('(')?.1.split_once(')')?.0.trim();
        let mut out = Vec::new();
        for part in inner
            .split(|c: char| c.is_whitespace() || c == ',' || c == '/')
            .filter(|p| !p.is_empty())
            .take(3)
        {
            let x = if let Some(p) = part.strip_suffix('%') {
                let x = p.parse::<f64>().ok()? * 2.55;
                if (x - x.trunc()).abs() == 0.5 {
                    2.0 * (x / 2.0).round() // Python rounds halves to even
                } else {
                    x.round()
                }
            } else {
                part.parse::<f64>().ok()?.trunc()
            };
            out.push(x.clamp(0.0, 255.0) as u8);
        }
        if out.len() == 3 {
            return Some(Paint::Rgb(out[0], out[1], out[2]));
        }
        return None;
    }
    // the Python engine knows the first 33 names; keep to those
    NAMED[..33]
        .iter()
        .find(|(n, _)| *n == v)
        .map(|(_, (r, g, b))| Paint::Rgb(*r, *g, *b))
}

/// What a stroke colour means in a pattern drawing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// Black: the outline of a printed piece.
    Outline,
    /// Blue: grout.
    Grout,
    /// Red / light red: a line direction, rings, or a literal path.
    Path,
}

pub fn stroke_role(p: Option<Paint>) -> Option<Role> {
    let Some(Paint::Rgb(r, g, b)) = p else {
        return None;
    };
    let (r, g, b) = (r as i32, g as i32, b as i32);
    if r.max(g).max(b) <= 64 {
        Some(Role::Outline)
    } else if b >= 128 && b - r.max(g) >= 64 {
        Some(Role::Grout)
    } else if r >= 128 && r - g.max(b) >= 48 {
        Some(Role::Path)
    } else {
        None
    }
}

/// `#rrggbb` (or `url` for a paint server): the key of a tile group.
pub fn color_key(p: Paint) -> String {
    match p {
        Paint::Url => "url".into(),
        Paint::Rgb(r, g, b) => format!("#{r:02x}{g:02x}{b:02x}"),
    }
}

// --------------------------------------------------------------------------- //
// Numbers, lengths, transforms
// --------------------------------------------------------------------------- //

/// The number starting at byte `i` of `s` (`[-+]?(\d+\.?\d*|\.\d+)([eE][-+]?\d+)?`): (value, end).
pub fn number_at(s: &[u8], i: usize) -> Option<(f64, usize)> {
    let mut j = i;
    if j < s.len() && (s[j] == b'+' || s[j] == b'-') {
        j += 1;
    }
    let digits = |mut k: usize| {
        while k < s.len() && s[k].is_ascii_digit() {
            k += 1;
        }
        k
    };
    let a = digits(j);
    let mut end;
    if a > j {
        end = a;
        if end < s.len() && s[end] == b'.' {
            end = digits(end + 1);
        }
    } else if j < s.len() && s[j] == b'.' && j + 1 < s.len() && s[j + 1].is_ascii_digit() {
        end = digits(j + 1);
    } else {
        return None;
    }
    if end < s.len() && (s[end] == b'e' || s[end] == b'E') {
        let mut k = end + 1;
        if k < s.len() && (s[k] == b'+' || s[k] == b'-') {
            k += 1;
        }
        let d = digits(k);
        if d > k {
            end = d;
        }
    }
    let text = std::str::from_utf8(&s[i..end]).ok()?;
    Some((text.parse().ok()?, end))
}

/// Every number in `s`, in order (like Python's NUM.findall).
pub fn numbers(s: &str) -> Vec<f64> {
    let b = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        match number_at(b, i) {
            Some((v, end)) => {
                out.push(v);
                i = end;
            }
            None => i += 1,
        }
    }
    out
}

pub fn unit_mm(unit: &str) -> Option<f64> {
    Some(match unit {
        "mm" => 1.0,
        "cm" => 10.0,
        "in" => 25.4,
        "pt" => 25.4 / 72.0,
        "pc" => 25.4 / 6.0,
        "px" | "" => 25.4 / 96.0,
        "q" => 0.25,
        _ => return None,
    })
}

/// `'12.5mm'` -> (12.5, "mm"); None if it doesn't start with a number.
pub fn parse_length(v: Option<&str>) -> Option<(f64, String)> {
    let s = v?;
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() && b[i].is_ascii_whitespace() {
        i += 1;
    }
    let (val, mut end) = number_at(b, i)?;
    while end < b.len() && b[end].is_ascii_whitespace() {
        end += 1;
    }
    let unit: String = s[end..]
        .chars()
        .take_while(|c| c.is_ascii_alphabetic() || *c == '%')
        .collect();
    Some((val, unit.to_ascii_lowercase()))
}

/// A 2D affine matrix (a, b, c, d, e, f) as in SVG: x' = a x + c y + e, y' = b x + d y + f.
pub type Mat = [f64; 6];
pub const IDENTITY: Mat = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];

pub fn mat_mul(a: &Mat, b: &Mat) -> Mat {
    [
        a[0] * b[0] + a[2] * b[1],
        a[1] * b[0] + a[3] * b[1],
        a[0] * b[2] + a[2] * b[3],
        a[1] * b[2] + a[3] * b[3],
        a[0] * b[4] + a[2] * b[5] + a[4],
        a[1] * b[4] + a[3] * b[5] + a[5],
    ]
}

pub fn parse_transform(s: Option<&str>) -> Mat {
    let mut m = IDENTITY;
    let Some(s) = s else { return m };
    let mut rest = s;
    while let Some(open) = rest.find('(') {
        let name: String = rest[..open]
            .trim_end()
            .chars()
            .rev()
            .take_while(|c| c.is_ascii_alphabetic())
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        let Some(close) = rest[open..].find(')') else {
            break;
        };
        let a = numbers(&rest[open + 1..open + close]);
        rest = &rest[open + close + 1..];
        let t: Mat = match (name.to_ascii_lowercase().as_str(), a.len()) {
            ("matrix", 6) => [a[0], a[1], a[2], a[3], a[4], a[5]],
            ("translate", n) if n >= 1 => {
                [1.0, 0.0, 0.0, 1.0, a[0], if n > 1 { a[1] } else { 0.0 }]
            }
            ("scale", n) if n >= 1 => [a[0], 0.0, 0.0, if n > 1 { a[1] } else { a[0] }, 0.0, 0.0],
            ("rotate", n) if n >= 1 => {
                let (s_, c) = a[0].to_radians().sin_cos();
                let r = [c, s_, -s_, c, 0.0, 0.0];
                if n >= 3 {
                    mat_mul(
                        &mat_mul(&[1.0, 0.0, 0.0, 1.0, a[1], a[2]], &r),
                        &[1.0, 0.0, 0.0, 1.0, -a[1], -a[2]],
                    )
                } else {
                    r
                }
            }
            ("skewx", n) if n >= 1 => [1.0, 0.0, a[0].to_radians().tan(), 1.0, 0.0, 0.0],
            ("skewy", n) if n >= 1 => [1.0, a[0].to_radians().tan(), 0.0, 1.0, 0.0, 0.0],
            _ => continue,
        };
        m = mat_mul(&m, &t);
    }
    m
}

/// Python's `%g`: 6 significant digits, trailing zeros dropped.
pub fn fmt_g(v: f64) -> String {
    if v == 0.0 {
        return if v.is_sign_negative() {
            "-0".into()
        } else {
            "0".into()
        };
    }
    if !v.is_finite() {
        return format!("{v}");
    }
    let e = format!("{v:.5e}");
    let (mant, exp) = e.split_once('e').unwrap();
    let exp: i32 = exp.parse().unwrap();
    if !(-4..6).contains(&exp) {
        let mant = if mant.contains('.') {
            mant.trim_end_matches('0').trim_end_matches('.')
        } else {
            mant
        };
        return format!(
            "{}e{}{:02}",
            mant,
            if exp < 0 { '-' } else { '+' },
            exp.abs()
        );
    }
    let decimals = (5 - exp).max(0) as usize;
    let s = format!("{v:.decimals$}");
    if s.contains('.') {
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    } else {
        s
    }
}

// --------------------------------------------------------------------------- //
// Path data
// --------------------------------------------------------------------------- //

struct Scan<'a> {
    d: &'a [u8],
    i: usize,
}

impl Scan<'_> {
    fn skip(&mut self) {
        while self.i < self.d.len() && matches!(self.d[self.i], b' ' | b',' | b'\t' | b'\r' | b'\n')
        {
            self.i += 1;
        }
    }

    fn cmd(&mut self) -> Option<u8> {
        self.skip();
        if self.i < self.d.len() && self.d[self.i].is_ascii_alphabetic() {
            self.i += 1;
            return Some(self.d[self.i - 1]);
        }
        None
    }

    fn has_num(&mut self) -> bool {
        self.skip();
        self.i < self.d.len()
            && (matches!(self.d[self.i], b'+' | b'-' | b'.') || self.d[self.i].is_ascii_digit())
    }

    fn near(&self) -> String {
        String::from_utf8_lossy(&self.d[self.i..(self.i + 20).min(self.d.len())]).into_owned()
    }

    fn num(&mut self) -> Result<f64, Error> {
        self.skip();
        match number_at(self.d, self.i) {
            Some((v, end)) => {
                self.i = end;
                Ok(v)
            }
            None => err(format!("bad number in path data near '{}'", self.near())),
        }
    }

    fn flag(&mut self) -> Result<bool, Error> {
        self.skip();
        match self.d.get(self.i) {
            Some(b'0') | Some(b'1') => {
                self.i += 1;
                Ok(self.d[self.i - 1] == b'1')
            }
            _ => err(format!("bad arc flag in path data near '{}'", self.near())),
        }
    }
}

fn bezier(pts: &[Pt], n: usize) -> Vec<Pt> {
    (1..=n)
        .map(|k| {
            let t = k as f64 / n as f64;
            let u = 1.0 - t;
            if pts.len() == 3 {
                let ((x0, y0), (x1, y1), (x2, y2)) = (pts[0], pts[1], pts[2]);
                (
                    u * u * x0 + 2.0 * u * t * x1 + t * t * x2,
                    u * u * y0 + 2.0 * u * t * y1 + t * t * y2,
                )
            } else {
                let ((x0, y0), (x1, y1), (x2, y2), (x3, y3)) = (pts[0], pts[1], pts[2], pts[3]);
                (
                    u.powi(3) * x0 + 3.0 * u * u * t * x1 + 3.0 * u * t * t * x2 + t.powi(3) * x3,
                    u.powi(3) * y0 + 3.0 * u * u * t * y1 + 3.0 * u * t * t * y2 + t.powi(3) * y3,
                )
            }
        })
        .collect()
}

fn nseg(pts: &[Pt], tol: f64) -> usize {
    let ln: f64 = pts
        .windows(2)
        .map(|w| (w[1].0 - w[0].0).hypot(w[1].1 - w[0].1))
        .sum();
    ((ln / tol).ceil() as i64).clamp(2, 400) as usize
}

/// An SVG elliptical arc as points (start point excluded).
#[allow(clippy::too_many_arguments)]
fn arc(
    x0: f64,
    y0: f64,
    rx: f64,
    ry: f64,
    phi: f64,
    large: bool,
    sweep: bool,
    x1: f64,
    y1: f64,
    tol: f64,
) -> Vec<Pt> {
    if rx == 0.0 || ry == 0.0 || (x0 == x1 && y0 == y1) {
        return vec![(x1, y1)];
    }
    let (mut rx, mut ry) = (rx.abs(), ry.abs());
    let (sp, cp) = phi.to_radians().sin_cos();
    let (dx, dy) = ((x0 - x1) / 2.0, (y0 - y1) / 2.0);
    let (x1p, y1p) = (cp * dx + sp * dy, -sp * dx + cp * dy);
    let lam = (x1p / rx).powi(2) + (y1p / ry).powi(2);
    if lam > 1.0 {
        rx *= lam.sqrt();
        ry *= lam.sqrt();
    }
    let num = rx * rx * ry * ry - rx * rx * y1p * y1p - ry * ry * x1p * x1p;
    let den = rx * rx * y1p * y1p + ry * ry * x1p * x1p;
    let mut co = if den != 0.0 {
        (num / den).max(0.0).sqrt()
    } else {
        0.0
    };
    if large == sweep {
        co = -co;
    }
    let (cxp, cyp) = (co * rx * y1p / ry, -co * ry * x1p / rx);
    let cx = cp * cxp - sp * cyp + (x0 + x1) / 2.0;
    let cy = sp * cxp + cp * cyp + (y0 + y1) / 2.0;
    let ang = |ux: f64, uy: f64, vx: f64, vy: f64| (ux * vy - uy * vx).atan2(ux * vx + uy * vy);
    let t1 = ang(1.0, 0.0, (x1p - cxp) / rx, (y1p - cyp) / ry);
    let mut dt = ang(
        (x1p - cxp) / rx,
        (y1p - cyp) / ry,
        (-x1p - cxp) / rx,
        (-y1p - cyp) / ry,
    );
    if !sweep && dt > 0.0 {
        dt -= 2.0 * PI;
    } else if sweep && dt < 0.0 {
        dt += 2.0 * PI;
    }
    let n = ((dt.abs() * rx.max(ry) / tol).ceil() as i64).clamp(2, 400) as usize;
    let mut out: Vec<Pt> = (1..=n)
        .map(|k| {
            let t = t1 + dt * k as f64 / n as f64;
            let (x, y) = (rx * t.cos(), ry * t.sin());
            (cp * x - sp * y + cx, sp * x + cp * y + cy)
        })
        .collect();
    *out.last_mut().unwrap() = (x1, y1);
    out
}

/// Path data to sub-paths in user units, curves flattened to within `tol`.
pub fn parse_path(d: &str, tol: f64) -> Result<Vec<Subpath>, Error> {
    let mut sc = Scan {
        d: d.as_bytes(),
        i: 0,
    };
    let mut subs: Vec<Subpath> = Vec::new();
    let mut cur: Vec<Pt> = Vec::new();
    let (mut x, mut y, mut sx, mut sy) = (0.0, 0.0, 0.0, 0.0);
    let mut last_c: Option<Pt> = None;
    let mut cmd: Option<u8> = None;
    let mut prev: Option<u8> = None;
    loop {
        let c = match sc.cmd() {
            Some(c) => c,
            None => {
                let Some(c0) = cmd else { break };
                if !sc.has_num() {
                    break;
                }
                match c0 {
                    b'M' => b'L',
                    b'm' => b'l',
                    c => c,
                }
            }
        };
        cmd = Some(c);
        let rel = c.is_ascii_lowercase();
        let big = c.to_ascii_uppercase();
        if big == b'Z' {
            if !cur.is_empty() {
                subs.push((std::mem::take(&mut cur), true));
            }
            x = sx;
            y = sy;
            last_c = None;
            prev = Some(big);
            cmd = None;
            continue;
        }
        if big == b'M' {
            if !cur.is_empty() {
                subs.push((std::mem::take(&mut cur), false));
            }
            let (mut nx, mut ny) = (sc.num()?, sc.num()?);
            if rel {
                nx += x;
                ny += y;
            }
            x = nx;
            y = ny;
            sx = nx;
            sy = ny;
            cur = vec![(x, y)];
            last_c = None;
        } else {
            if cur.is_empty() {
                cur.push((x, y));
            }
            match big {
                b'L' => {
                    let (mut nx, mut ny) = (sc.num()?, sc.num()?);
                    if rel {
                        nx += x;
                        ny += y;
                    }
                    x = nx;
                    y = ny;
                    cur.push((x, y));
                    last_c = None;
                }
                b'H' => {
                    let nx = sc.num()?;
                    x = if rel { nx + x } else { nx };
                    cur.push((x, y));
                    last_c = None;
                }
                b'V' => {
                    let ny = sc.num()?;
                    y = if rel { ny + y } else { ny };
                    cur.push((x, y));
                    last_c = None;
                }
                b'C' | b'S' => {
                    let (x1, y1) = if big == b'C' {
                        let (mut a, mut b) = (sc.num()?, sc.num()?);
                        if rel {
                            a += x;
                            b += y;
                        }
                        (a, b)
                    } else {
                        match (prev, last_c) {
                            (Some(b'C' | b'S'), Some(lc)) => (2.0 * x - lc.0, 2.0 * y - lc.1),
                            _ => (x, y),
                        }
                    };
                    let (mut x2, mut y2, mut nx, mut ny) =
                        (sc.num()?, sc.num()?, sc.num()?, sc.num()?);
                    if rel {
                        x2 += x;
                        y2 += y;
                        nx += x;
                        ny += y;
                    }
                    let pts = [(x, y), (x1, y1), (x2, y2), (nx, ny)];
                    cur.extend(bezier(&pts, nseg(&pts, tol)));
                    last_c = Some((x2, y2));
                    x = nx;
                    y = ny;
                }
                b'Q' | b'T' => {
                    let (x1, y1) = if big == b'Q' {
                        let (mut a, mut b) = (sc.num()?, sc.num()?);
                        if rel {
                            a += x;
                            b += y;
                        }
                        (a, b)
                    } else {
                        match (prev, last_c) {
                            (Some(b'Q' | b'T'), Some(lc)) => (2.0 * x - lc.0, 2.0 * y - lc.1),
                            _ => (x, y),
                        }
                    };
                    let (mut nx, mut ny) = (sc.num()?, sc.num()?);
                    if rel {
                        nx += x;
                        ny += y;
                    }
                    let pts = [(x, y), (x1, y1), (nx, ny)];
                    cur.extend(bezier(&pts, nseg(&pts, tol)));
                    last_c = Some((x1, y1));
                    x = nx;
                    y = ny;
                }
                b'A' => {
                    let (rx, ry, phi) = (sc.num()?, sc.num()?, sc.num()?);
                    let (large, sweep) = (sc.flag()?, sc.flag()?);
                    let (mut nx, mut ny) = (sc.num()?, sc.num()?);
                    if rel {
                        nx += x;
                        ny += y;
                    }
                    cur.extend(arc(x, y, rx, ry, phi, large, sweep, nx, ny, tol));
                    x = nx;
                    y = ny;
                    last_c = None;
                }
                _ => return err(format!("unsupported path command '{}'", c as char)),
            }
        }
        prev = Some(big);
    }
    if cur.len() > 1 {
        subs.push((cur, false));
    }
    Ok(subs)
}

fn ellipse(cx: f64, cy: f64, rx: f64, ry: f64, tol: f64) -> Vec<Pt> {
    let n = ((2.0 * PI * rx.max(ry) / tol).ceil() as i64).clamp(24, 720) as usize;
    (0..n)
        .map(|k| {
            let a = 2.0 * PI * k as f64 / n as f64;
            (cx + rx * a.cos(), cy + ry * a.sin())
        })
        .collect()
}

// --------------------------------------------------------------------------- //
// Styles
// --------------------------------------------------------------------------- //

type Style = HashMap<String, String>;

fn parse_style(s: Option<&str>) -> Style {
    let mut out = Style::new();
    for part in s.unwrap_or("").split(';') {
        if let Some((k, v)) = part.split_once(':') {
            out.insert(k.trim().to_ascii_lowercase(), v.trim().to_string());
        }
    }
    out
}

fn parse_css(text: &str) -> Vec<(String, Style)> {
    // drop /* comments */
    let mut clean = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(a) = rest.find("/*") {
        clean.push_str(&rest[..a]);
        match rest[a + 2..].find("*/") {
            Some(b) => rest = &rest[a + 2 + b + 2..],
            None => {
                rest = "";
                break;
            }
        }
    }
    clean.push_str(rest);
    let mut rules = Vec::new();
    let mut rest = clean.as_str();
    while let Some(open) = rest.find('{') {
        let sel = &rest[..open];
        let Some(close) = rest[open..].find('}') else {
            break;
        };
        let body = &rest[open + 1..open + close];
        rest = &rest[open + close + 1..];
        // a selector is what follows the previous rule ([^{}]+)
        let sel = sel.rsplit('}').next().unwrap_or(sel);
        let decl = parse_style(Some(body));
        for s in sel.split(',') {
            let s = s.trim();
            if !s.is_empty() {
                rules.push((s.to_string(), decl.clone()));
            }
        }
    }
    rules
}

const INHERITED: [&str; 5] = ["fill", "stroke", "stroke-width", "display", "visibility"];
const SKIPPED: [&str; 16] = [
    "defs",
    "clipPath",
    "mask",
    "symbol",
    "marker",
    "pattern",
    "linearGradient",
    "radialGradient",
    "style",
    "title",
    "desc",
    "metadata",
    "text",
    "image",
    "foreignObject",
    "filter",
];

/// One drawn shape, in mm.
#[derive(Debug, Clone)]
pub struct Item {
    pub fill: Option<Paint>,
    pub stroke: Option<Paint>,
    pub stroke_mm: f64,
    pub subpaths: Vec<Subpath>,
    /// Drawing order: later shapes cover earlier ones.
    pub order: usize,
    pub id: String,
    pub tag: String,
}

/// A drawing read into mm.
#[derive(Debug, Clone)]
pub struct Drawing {
    pub items: Vec<Item>,
    /// Page size in mm.
    pub size: (f64, f64),
    /// mm per user unit (x).
    pub unit_mm: f64,
    /// `data-pattern-rotate` / `data-pattern-origin` on the root (layouts).
    pub pattern_rotate: Option<f64>,
    pub pattern_origin: Option<(f64, f64)>,
}

struct Reader<'a> {
    doc: &'a roxmltree::Document<'a>,
    css: Vec<(String, Style)>,
    ids: HashMap<&'a str, roxmltree::Node<'a, 'a>>,
    vb: [f64; 4],
    sx: f64,
    sy: f64,
    tol_u: f64,
    items: Vec<Item>,
}

const XLINK: &str = "http://www.w3.org/1999/xlink";

impl<'a> Reader<'a> {
    fn css_for(&self, el: roxmltree::Node) -> Style {
        let tag = el.tag_name().name();
        let classes: Vec<&str> = el
            .attribute("class")
            .unwrap_or("")
            .split_whitespace()
            .collect();
        let eid = el.attribute("id");
        let mut out = Style::new();
        for rank in 0..3 {
            for (sel, decl) in &self.css {
                let hit = match rank {
                    0 => sel == tag,
                    1 => {
                        (sel.starts_with('.') && classes.contains(&&sel[1..]))
                            || sel.split_once('.').is_some_and(|(t, c)| {
                                !t.is_empty() && t == tag && classes.contains(&c)
                            })
                    }
                    _ => sel.strip_prefix('#').is_some_and(|i| Some(i) == eid),
                };
                if hit {
                    out.extend(decl.iter().map(|(k, v)| (k.clone(), v.clone())));
                }
            }
        }
        out
    }

    fn style_of(&self, el: roxmltree::Node, parent: &Style) -> Style {
        let mut st: Style = INHERITED
            .iter()
            .filter_map(|k| parent.get(*k).map(|v| (k.to_string(), v.clone())))
            .collect();
        for k in [
            "fill",
            "stroke",
            "stroke-width",
            "display",
            "visibility",
            "opacity",
            "fill-opacity",
            "stroke-opacity",
        ] {
            if let Some(v) = el.attribute(k) {
                st.insert(k.to_string(), v.to_string());
            }
        }
        st.extend(self.css_for(el));
        st.extend(parse_style(el.attribute("style")));
        st
    }

    fn to_mm(&self, m: &Mat, pts: &[Pt]) -> Vec<Pt> {
        pts.iter()
            .map(|&(x, y)| {
                let (xx, yy) = (m[0] * x + m[2] * y + m[4], m[1] * x + m[3] * y + m[5]);
                (
                    (xx - self.vb[0]) * self.sx,
                    (self.vb[1] + self.vb[3] - yy) * self.sy,
                )
            })
            .collect()
    }

    fn emit(&mut self, el: roxmltree::Node, m: &Mat, st: &Style, subs: Vec<Subpath>) {
        let opacity = |keys: &[&str]| -> f64 {
            let mut v = 1.0;
            for k in keys {
                let raw = st
                    .get(*k)
                    .map(|s| s.trim().to_string())
                    .unwrap_or_else(|| "1".into());
                let x = if let Some(p) = raw.strip_suffix('%') {
                    p.parse::<f64>().map(|p| p / 100.0)
                } else {
                    raw.parse::<f64>()
                };
                if let Ok(x) = x {
                    v *= x;
                }
            }
            v
        };
        let mut fill = parse_color(Some(st.get("fill").map(String::as_str).unwrap_or("black")));
        let mut stroke = parse_color(Some(st.get("stroke").map(String::as_str).unwrap_or("none")));
        if opacity(&["opacity", "fill-opacity"]) <= 0.01 {
            fill = None;
        }
        if opacity(&["opacity", "stroke-opacity"]) <= 0.01 {
            stroke = None;
        }
        let (sw, unit) = parse_length(st.get("stroke-width").map(String::as_str))
            .unwrap_or((1.0, String::new()));
        let factor = match unit_mm(&unit) {
            Some(u) if !unit.is_empty() && unit != "px" => 96.0 / 25.4 * u,
            _ => 1.0,
        };
        let det = (m[0] * m[3] - m[1] * m[2]).abs();
        let stroke_mm = sw * factor * det.sqrt() * (self.sx * self.sy).sqrt();
        let subs_mm: Vec<Subpath> = subs
            .into_iter()
            .filter(|(p, _)| p.len() >= 2)
            .map(|(p, c)| (self.to_mm(m, &p), c))
            .collect();
        if !subs_mm.is_empty() {
            let order = self.items.len();
            self.items.push(Item {
                fill,
                stroke,
                stroke_mm,
                subpaths: subs_mm,
                order,
                id: el.attribute("id").unwrap_or("").to_string(),
                tag: el.tag_name().name().to_string(),
            });
        }
    }

    fn walk(
        &mut self,
        el: roxmltree::Node<'a, 'a>,
        m: &Mat,
        parent_st: &Style,
        depth: usize,
    ) -> Result<(), Error> {
        if depth > 40 || !el.is_element() {
            return Ok(());
        }
        let tag = el.tag_name().name();
        if SKIPPED.contains(&tag) {
            return Ok(());
        }
        let st = self.style_of(el, parent_st);
        if st.get("display").is_some_and(|d| d.trim() == "none")
            || st
                .get("visibility")
                .is_some_and(|v| matches!(v.trim(), "hidden" | "collapse"))
        {
            return Ok(());
        }
        let mut m = mat_mul(m, &parse_transform(el.attribute("transform")));
        let f = |name: &str, dflt: f64| {
            parse_length(el.attribute(name))
                .map(|v| v.0)
                .unwrap_or(dflt)
        };
        if (["svg", "g", "a", "switch"].contains(&tag) && depth > 0) || tag == "svg" {
            if tag == "svg" && depth > 0 {
                m = mat_mul(&m, &[1.0, 0.0, 0.0, 1.0, f("x", 0.0), f("y", 0.0)]);
            }
            for ch in el.children() {
                self.walk(ch, &m, &st, depth + 1)?;
            }
            return Ok(());
        }
        if tag == "use" {
            let href = el
                .attribute((XLINK, "href"))
                .or_else(|| el.attribute("href"))
                .unwrap_or("");
            if let Some(r) = self.ids.get(href.trim_start_matches('#')).copied() {
                let mm = mat_mul(&m, &[1.0, 0.0, 0.0, 1.0, f("x", 0.0), f("y", 0.0)]);
                if r.tag_name().name() == "symbol" {
                    for ch in r.children() {
                        self.walk(ch, &mm, &st, depth + 1)?;
                    }
                } else {
                    self.walk(r, &mm, &st, depth + 1)?;
                }
            }
            return Ok(());
        }
        let tol = self.tol_u;
        let subs: Option<Vec<Subpath>> = match tag {
            "path" => Some(parse_path(el.attribute("d").unwrap_or(""), tol)?),
            "rect" => {
                let (x, y, w, h) = (f("x", 0.0), f("y", 0.0), f("width", 0.0), f("height", 0.0));
                if w <= 0.0 || h <= 0.0 {
                    return Ok(());
                }
                let rx0 = parse_length(el.attribute("rx")).map(|v| v.0);
                let ry0 = parse_length(el.attribute("ry")).map(|v| v.0);
                let (rx, ry) = (rx0.or(ry0), ry0.or(rx0));
                let rx = rx.unwrap_or(0.0).min(w / 2.0);
                let ry = ry.unwrap_or(0.0).min(h / 2.0);
                if rx > 0.0 && ry > 0.0 {
                    let g = fmt_g;
                    let d = format!(
                        "M{},{} H{} A{},{} 0 0 1 {},{} V{} A{},{} 0 0 1 {},{} H{} A{},{} 0 0 1 {},{} V{} A{},{} 0 0 1 {},{} Z",
                        g(x + rx), g(y), g(x + w - rx), g(rx), g(ry), g(x + w), g(y + ry), g(y + h - ry),
                        g(rx), g(ry), g(x + w - rx), g(y + h), g(x + rx), g(rx), g(ry), g(x), g(y + h - ry),
                        g(y + ry), g(rx), g(ry), g(x + rx), g(y)
                    );
                    Some(parse_path(&d, tol)?)
                } else {
                    Some(vec![(
                        vec![(x, y), (x + w, y), (x + w, y + h), (x, y + h)],
                        true,
                    )])
                }
            }
            "circle" => {
                let r = f("r", 0.0);
                (r > 0.0).then(|| vec![(ellipse(f("cx", 0.0), f("cy", 0.0), r, r, tol), true)])
            }
            "ellipse" => {
                let (rx, ry) = (f("rx", 0.0), f("ry", 0.0));
                (rx > 0.0 && ry > 0.0)
                    .then(|| vec![(ellipse(f("cx", 0.0), f("cy", 0.0), rx, ry, tol), true)])
            }
            "line" => Some(vec![(
                vec![(f("x1", 0.0), f("y1", 0.0)), (f("x2", 0.0), f("y2", 0.0))],
                false,
            )]),
            "polyline" | "polygon" => {
                let v = numbers(el.attribute("points").unwrap_or(""));
                let pts: Vec<Pt> = v.chunks_exact(2).map(|c| (c[0], c[1])).collect();
                Some(vec![(pts, tag == "polygon")])
            }
            _ => None,
        };
        if let Some(subs) = subs {
            if !subs.is_empty() {
                self.emit(el, &m, &st, subs);
            }
        }
        Ok(())
    }
}

/// Read an SVG drawing (text). `scale` forces mm per user unit; `tol_mm` is
/// how closely curves are followed.
pub fn read_svg(text: &str, scale: Option<f64>, tol_mm: f64) -> Result<Drawing, Error> {
    let opts = roxmltree::ParsingOptions {
        allow_dtd: true,
        ..Default::default()
    };
    let doc = roxmltree::Document::parse_with_options(text, opts)
        .map_err(|e| Error(format!("can't read SVG: {e}")))?;
    let root = doc.root_element();
    if root.tag_name().name() != "svg" {
        return err("not an SVG file");
    }
    let mut vb = numbers(root.attribute("viewBox").unwrap_or(""));
    let wl = parse_length(root.attribute("width"));
    let hl = parse_length(root.attribute("height"));
    if vb.len() != 4 {
        let w = wl
            .as_ref()
            .filter(|l| l.1 != "%")
            .map(|l| l.0)
            .unwrap_or(100.0);
        let h = hl
            .as_ref()
            .filter(|l| l.1 != "%")
            .map(|l| l.0)
            .unwrap_or(100.0);
        vb = vec![0.0, 0.0, w, h];
    }
    let (sx, sy) = match scale {
        Some(s) => (s, s),
        None => {
            let mm_per = |l: &Option<(f64, String)>, span: f64| match l {
                Some((v, u)) if u != "%" && unit_mm(u).is_some() => v * unit_mm(u).unwrap() / span,
                _ => 25.4 / 96.0,
            };
            let sx = mm_per(&wl, vb[2]);
            let sy = if hl.is_some() { mm_per(&hl, vb[3]) } else { sx };
            (sx, sy)
        }
    };
    let tol_u = tol_mm / sx.max(sy);
    let mut css = Vec::new();
    let mut ids = HashMap::new();
    for n in doc.descendants().filter(|n| n.is_element()) {
        if n.tag_name().name() == "style" {
            let text: String = n.children().filter_map(|c| c.text()).collect();
            css.extend(parse_css(&text));
        }
        if let Some(id) = n.attribute("id") {
            ids.insert(id, n);
        }
    }
    let mut r = Reader {
        doc: &doc,
        css,
        ids,
        vb: [vb[0], vb[1], vb[2], vb[3]],
        sx,
        sy,
        tol_u,
        items: Vec::new(),
    };
    let _ = r.doc;
    let base: Style = [("fill", "black"), ("stroke", "none"), ("stroke-width", "1")]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    r.walk(root, &IDENTITY, &base, 0)?;
    let rot = match root.attribute("data-pattern-rotate") {
        None | Some("") => None,
        Some(v) => Some(
            v.trim()
                .parse::<f64>()
                .map_err(|_| Error("data-pattern-rotate must be a number".into()))?,
        ),
    };
    let org = numbers(root.attribute("data-pattern-origin").unwrap_or(""));
    Ok(Drawing {
        items: r.items,
        size: (vb[2] * sx, vb[3] * sy),
        unit_mm: sx,
        pattern_rotate: rot,
        pattern_origin: (org.len() >= 2).then(|| (org[0], org[1])),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colours() {
        assert_eq!(parse_color(Some("#f00")), Some(Paint::Rgb(255, 0, 0)));
        assert_eq!(parse_color(Some("#0000FFcc")), Some(Paint::Rgb(0, 0, 255)));
        assert_eq!(
            parse_color(Some("rgb(100%, 50%, 0)")),
            Some(Paint::Rgb(255, 127, 0))
        ); // as Python: round(127.4999...)
        assert_eq!(parse_color(Some("url(#g)")), Some(Paint::Url));
        assert_eq!(parse_color(Some("none")), None);
        assert_eq!(parse_color(Some("Tomato")), Some(Paint::Rgb(255, 99, 71)));
        assert_eq!(stroke_role(Some(Paint::Rgb(0, 0, 255))), Some(Role::Grout));
        assert_eq!(
            stroke_role(Some(Paint::Rgb(240, 128, 128))),
            Some(Role::Path)
        );
        assert_eq!(
            stroke_role(Some(Paint::Rgb(20, 20, 20))),
            Some(Role::Outline)
        );
        assert_eq!(color_key(Paint::Rgb(216, 176, 122)), "#d8b07a");
    }

    #[test]
    fn numbers_and_lengths() {
        assert_eq!(numbers("M1,2.5-3e2 .5.5"), vec![1.0, 2.5, -300.0, 0.5, 0.5]);
        assert_eq!(
            parse_length(Some(" 12.5mm")),
            Some((12.5, "mm".to_string()))
        );
        assert_eq!(parse_length(Some("50%")), Some((50.0, "%".to_string())));
        assert_eq!(parse_length(Some("auto")), None);
        assert_eq!(fmt_g(12.3456789), "12.3457");
        assert_eq!(fmt_g(0.5), "0.5");
        assert_eq!(fmt_g(100.0), "100");
        assert_eq!(fmt_g(1234567.0), "1.23457e+06");
        assert_eq!(fmt_g(0.00001234), "1.234e-05");
    }

    #[test]
    fn transforms() {
        let m = parse_transform(Some("translate(10, 5) scale(2)"));
        assert_eq!(m, [2.0, 0.0, 0.0, 2.0, 10.0, 5.0]);
        let r = parse_transform(Some("rotate(90 1 1)"));
        let (x, y) = (
            r[0] * 2.0 + r[2] * 1.0 + r[4],
            r[1] * 2.0 + r[3] * 1.0 + r[5],
        );
        assert!((x - 1.0).abs() < 1e-12 && (y - 2.0).abs() < 1e-12);
    }

    #[test]
    fn paths() {
        let s = parse_path("M0 0 L10 0 l0 10 H0 Z m20 0 h5 v5", 0.1).unwrap();
        assert_eq!(s.len(), 2);
        assert_eq!(
            s[0],
            (
                vec![(0.0, 0.0), (10.0, 0.0), (10.0, 10.0), (0.0, 10.0)],
                true
            )
        );
        assert_eq!(s[1], (vec![(20.0, 0.0), (25.0, 0.0), (25.0, 5.0)], false));
        let arc = parse_path("M0 0 A5 5 0 0 1 10 0", 0.1).unwrap();
        let last = *arc[0].0.last().unwrap();
        assert_eq!(last, (10.0, 0.0));
        assert!(arc[0]
            .0
            .iter()
            .all(|p| ((p.0 - 5.0).hypot(p.1) - 5.0).abs() < 1e-9));
        assert!(parse_path("M0 0 X1", 0.1).is_err());
    }

    #[test]
    fn reads_a_drawing() {
        let svg = r##"<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink"
             width="20mm" height="10mm" viewBox="0 0 20 10" data-pattern-rotate="45" data-pattern-origin="1 2">
          <style>.tile { fill: #d8b07a } #g1 { stroke-width: 0.4 }</style>
          <defs><rect id="r" width="4" height="2"/></defs>
          <g stroke="#0000ff">
            <rect class="tile" x="0" y="0" width="10" height="10"/>
            <use xlink:href="#r" x="12" y="1" style="fill:#9c6a3c"/>
            <path id="g1" fill="none" d="M10 0 V10"/>
          </g>
          <line x1="1" y1="5" x2="9" y2="5" stroke="#f08080"/>
          <circle cx="15" cy="7" r="1" display="none"/>
        </svg>"##;
        let d = read_svg(svg, None, 0.15).unwrap();
        assert_eq!(d.size, (20.0, 10.0));
        assert_eq!(d.pattern_rotate, Some(45.0));
        assert_eq!(d.pattern_origin, Some((1.0, 2.0)));
        assert_eq!(d.items.len(), 4);
        let tile = &d.items[0];
        assert_eq!(tile.fill, Some(Paint::Rgb(0xd8, 0xb0, 0x7a)));
        assert_eq!(tile.stroke, Some(Paint::Rgb(0, 0, 255)));
        // y flipped: the rect's top edge (y=0) is at 10 mm
        assert_eq!(tile.subpaths[0].0[0], (0.0, 10.0));
        let used = &d.items[1];
        assert_eq!(used.fill, Some(Paint::Rgb(0x9c, 0x6a, 0x3c)));
        assert_eq!(used.subpaths[0].0[0], (12.0, 9.0));
        let grout = &d.items[2];
        assert_eq!(grout.fill, None);
        assert!((grout.stroke_mm - 0.4).abs() < 1e-12);
        assert_eq!(stroke_role(d.items[3].stroke), Some(Role::Path));
    }
}
