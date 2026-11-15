//! The provided patterns: their shapes, parameter forms and presets. A port
//! of pattern_library.py.
//!
//! A pattern is a tile design. One repeat of it, drawn 1:1 in mm, is an SVG
//! whose page is the repeat: it is copied every page-width across and every
//! page-height up, so a tile hanging over the page edge is completed by the
//! copy next to it. Fill colour = a tile (a tile group); blue stroke = grout;
//! light red line = line direction for that colour (a closed loop = rings).
//!
//! Names, tile groups, Form.io parameter forms and presets are data
//! (`library.json`, generated from the Python registry); the drawing of each
//! pattern from its parameter values is code, here.

use crate::direction::Direction;
use crate::form;
use crate::geom::{self, MP};
use crate::svg::fmt_g;
use geo::{Area, LineString, MultiPolygon, Polygon};
use serde_json::{Map, Value};
use std::f64::consts::{PI, SQRT_2};
use std::sync::OnceLock;

const R2: f64 = SQRT_2;
const R3: f64 = 1.7320508075688772; // sqrt(3)
const BLUE: &str = "#0000ff";
const RED: &str = "#ff8080";

const LIGHT: &str = "#d8b07a";
const DARK: &str = "#9c6a3c";
const MID: &str = "#c48a50";
const BRICK: &str = "#b5562f";
const HONEY: &str = "#e0b040";
const BLOCKS: [&str; 3] = ["#f0dcb4", "#b4783c", "#784620"];
const AQUA: &str = "#8ec3c9";
const TEAL: &str = "#3f7f8c";
const CREAM: &str = "#efe3c8";
const SLATE: &str = "#5f6b75";
const CLAY: &str = "#c4744f";
const SAND: &str = "#d9c49a";
const NAVY: &str = "#2f4a6b";

#[derive(Debug, Clone, PartialEq)]
pub struct Error(pub String);

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Error {}

fn err<T>(m: impl Into<String>) -> Result<T, Error> {
    Err(Error(m.into()))
}

pub type Pt = (f64, f64);

/// One repeat of a pattern at given parameter values.
#[derive(Debug, Clone)]
pub struct Built {
    pub name: String,
    pub about: String,
    pub cell: (f64, f64),
    pub tiles: Vec<(Vec<Pt>, &'static str)>,
    pub grout: f64,
    /// Line direction per tile group, in order.
    pub dirs: Vec<(String, Direction)>,
    pub notes: Vec<String>,
}

// --------------------------------------------------------------------------- //
// Python formatting (the SVG text must read the same)
// --------------------------------------------------------------------------- //

/// Python's `repr(float)`: shortest round-trip digits, `6.0`, `1e-05`, `1.5e+16`.
pub fn py_float(x: f64) -> String {
    if x.is_nan() {
        return "NaN".into();
    }
    if x.is_infinite() {
        return if x > 0.0 {
            "Infinity".into()
        } else {
            "-Infinity".into()
        };
    }
    if x == 0.0 {
        return if x.is_sign_negative() {
            "-0.0".into()
        } else {
            "0.0".into()
        };
    }
    let e = format!("{x:e}"); // shortest digits, e.g. "1.2345e-5"
    let (mant, exp) = e.split_once('e').unwrap();
    let exp: i32 = exp.parse().unwrap();
    let neg = mant.starts_with('-');
    let digits: String = mant.chars().filter(|c| c.is_ascii_digit()).collect();
    let sign = if neg { "-" } else { "" };
    if (-4..16).contains(&exp) {
        // positional
        let point = exp + 1; // digits before the decimal point
        let s = if point <= 0 {
            format!("0.{}{}", "0".repeat((-point) as usize), digits)
        } else if point as usize >= digits.len() {
            format!("{}{}.0", digits, "0".repeat(point as usize - digits.len()))
        } else {
            format!(
                "{}.{}",
                &digits[..point as usize],
                &digits[point as usize..]
            )
        };
        format!("{sign}{s}")
    } else {
        let m = if digits.len() > 1 {
            format!("{}.{}", &digits[..1], &digits[1..])
        } else {
            digits
        };
        format!(
            "{sign}{m}e{}{:02}",
            if exp < 0 { '-' } else { '+' },
            exp.abs()
        )
    }
}

/// `json.dumps(v, sort_keys=True)` as Python writes it.
pub fn py_json(v: &Value) -> String {
    match v {
        Value::Null => "null".into(),
        Value::Bool(b) => if *b { "true" } else { "false" }.into(),
        Value::Number(n) => match (n.as_i64(), n.is_f64()) {
            (Some(i), false) => i.to_string(),
            _ => py_float(n.as_f64().unwrap_or(f64::NAN)),
        },
        Value::String(s) => {
            let mut out = String::from("\"");
            for c in s.chars() {
                match c {
                    '"' => out.push_str("\\\""),
                    '\\' => out.push_str("\\\\"),
                    '\n' => out.push_str("\\n"),
                    '\r' => out.push_str("\\r"),
                    '\t' => out.push_str("\\t"),
                    c if (c as u32) < 0x20 || (c as u32) > 0x7e => {
                        let mut buf = [0u16; 2];
                        for u in c.encode_utf16(&mut buf) {
                            out.push_str(&format!("\\u{:04x}", u));
                        }
                    }
                    c => out.push(c),
                }
            }
            out.push('"');
            out
        }
        Value::Array(a) => format!("[{}]", a.iter().map(py_json).collect::<Vec<_>>().join(", ")),
        Value::Object(m) => {
            let mut keys: Vec<&String> = m.keys().collect();
            keys.sort();
            let parts: Vec<String> = keys
                .iter()
                .map(|k| {
                    format!(
                        "{}: {}",
                        py_json(&Value::String((*k).clone())),
                        py_json(&m[*k])
                    )
                })
                .collect();
            format!("{{{}}}", parts.join(", "))
        }
    }
}

/// `html.escape(s, quote=True)`.
fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#x27;")
}

/// Python's `%.Ng` (N significant digits).
fn fmt_gp(v: f64, prec: usize) -> String {
    if v == 0.0 {
        return "0".into();
    }
    let e = format!("{:.*e}", prec - 1, v);
    let (mant, exp) = e.split_once('e').unwrap();
    let exp: i32 = exp.parse().unwrap();
    if exp < -4 || exp >= prec as i32 {
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
    let decimals = (prec as i32 - 1 - exp).max(0) as usize;
    let s = format!("{v:.decimals$}");
    if s.contains('.') {
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    } else {
        s
    }
}

fn g(v: f64) -> String {
    fmt_g(v)
}

// --------------------------------------------------------------------------- //
// Geometry helpers
// --------------------------------------------------------------------------- //

fn rect(x: f64, y: f64, w: f64, h: f64) -> Vec<Pt> {
    vec![(x, y), (x + w, y), (x + w, y + h), (x, y + h)]
}

fn rot(pts: &[Pt], deg: f64) -> Vec<Pt> {
    let (s, c) = deg.to_radians().sin_cos();
    pts.iter()
        .map(|&(x, y)| (x * c - y * s, x * s + y * c))
        .collect()
}

fn mv(pts: &[Pt], dx: f64, dy: f64) -> Vec<Pt> {
    pts.iter().map(|&(x, y)| (x + dx, y + dy)).collect()
}

fn centroid(pts: &[Pt]) -> Pt {
    let (mut a, mut cx, mut cy) = (0.0, 0.0, 0.0);
    for i in 0..pts.len() {
        let (x0, y0) = pts[i];
        let (x1, y1) = pts[(i + 1) % pts.len()];
        let k = x0 * y1 - x1 * y0;
        a += k;
        cx += (x0 + x1) * k;
        cy += (y0 + y1) * k;
    }
    a /= 2.0;
    (cx / (6.0 * a), cy / (6.0 * a))
}

fn inside(pts: &[Pt], x: f64, y: f64) -> bool {
    let mut c = false;
    for i in 0..pts.len() {
        let (x0, y0) = pts[i];
        let (x1, y1) = pts[(i + 1) % pts.len()];
        if (y0 > y) != (y1 > y) && x < x0 + (y - y0) * (x1 - x0) / (y1 - y0) {
            c = !c;
        }
    }
    c
}

/// Segment through (cx, cy) at `deg`, kept `margin` inside the polygon.
fn chord(pts: &[Pt], cx: f64, cy: f64, deg: f64, margin: f64) -> (Pt, Pt) {
    let (uy, ux) = deg.to_radians().sin_cos();
    let mut ends = [0.0; 2];
    for (k, sign) in [1.0, -1.0].iter().enumerate() {
        let mut t = 0.0;
        while inside(
            pts,
            cx + sign * (t + 0.05) * ux,
            cy + sign * (t + 0.05) * uy,
        ) {
            t += 0.05;
        }
        ends[k] = (t - margin).max(0.0);
    }
    (
        (cx - ends[1] * ux, cy - ends[1] * uy),
        (cx + ends[0] * ux, cy + ends[0] * uy),
    )
}

/// Copies of the base tiles that belong to one repeat cell (every tile whose
/// centre falls in the cell is kept once).
fn periodic(
    base: &[(Vec<Pt>, &'static str)],
    lattice: [Pt; 2],
    cell: Pt,
) -> Vec<(Vec<Pt>, &'static str)> {
    let [(ax, ay), (bx, by)] = lattice;
    let (cw, ch) = cell;
    let n = 40;
    let mut out = Vec::new();
    for (pts, col) in base {
        let (cx, cy) = centroid(pts);
        for i in -n..=n {
            for j in -n..=n {
                let (i, j) = (i as f64, j as f64);
                let (tx, ty) = (i * ax + j * bx, j * by + i * ay);
                let (x, y) = (cx + tx, cy + ty);
                if -1e-7 <= x && x < cw - 1e-7 && -1e-7 <= y && y < ch - 1e-7 {
                    out.push((mv(pts, tx, ty), *col));
                }
            }
        }
    }
    out
}

fn herringbone_45(w: f64, n: f64, origin: Pt) -> (Vec<(Vec<Pt>, &'static str)>, Pt) {
    let l = n * w;
    let h = mv(&rot(&rect(0.0, 0.0, l, w), -45.0), origin.0, origin.1);
    let v = mv(
        &rot(&rect(n * w, (1.0 - n) * w, w, l), -45.0),
        origin.0,
        origin.1,
    );
    let cell = (w * R2, n * w * R2);
    (
        periodic(
            &[(h, LIGHT), (v, DARK)],
            [(cell.0, 0.0), (0.0, cell.1)],
            cell,
        ),
        cell,
    )
}

/// A circle as shapely's Point.buffer(r, quad_segs=16) draws it (64 sides).
fn disc(cx: f64, cy: f64, r: f64) -> Polygon<f64> {
    let pts: Vec<Pt> = (0..64)
        .map(|k| {
            let a = 2.0 * PI * k as f64 / 64.0;
            (cx + r * a.cos(), cy + r * a.sin())
        })
        .collect();
    poly(&pts)
}

fn poly(pts: &[Pt]) -> Polygon<f64> {
    let mut v = pts.to_vec();
    v.push(pts[0]);
    Polygon::new(LineString::from(v), vec![])
}

fn mp(p: Polygon<f64>) -> MP {
    MultiPolygon::new(vec![p])
}

/// Exterior points of the largest part of a shape.
fn outline_of(g: &MP) -> Result<Vec<Pt>, Error> {
    let p = g
        .iter()
        .max_by(|a, b| a.unsigned_area().partial_cmp(&b.unsigned_area()).unwrap())
        .ok_or_else(|| Error("the pattern's shapes came out empty".into()))?;
    let c = &p.exterior().0;
    Ok(c[..c.len() - 1].iter().map(|q| (q.x, q.y)).collect())
}

/// `poly` with matching curves on pairs of opposite edges, so it still tiles by translation.
fn bend(polyg: &[Pt], pairs: &[(usize, usize, Vec<Pt>)]) -> Vec<Pt> {
    let n = polyg.len();
    let mut edges: Vec<Option<Vec<Pt>>> = vec![None; n];
    for (i, j, profile) in pairs {
        let (x0, y0) = polyg[*i];
        let (x1, y1) = polyg[(i + 1) % n];
        let (dx, dy) = (x1 - x0, y1 - y0);
        let pts: Vec<Pt> = profile
            .iter()
            .map(|&(u, v)| (x0 + u * dx - v * dy, y0 + u * dy + v * dx))
            .collect();
        let (tx, ty) = (polyg[*j].0 - x1, polyg[*j].1 - y1);
        edges[*j] = Some(pts.iter().rev().map(|&(x, y)| (x + tx, y + ty)).collect());
        edges[*i] = Some(pts);
    }
    let mut out = Vec::new();
    for k in 0..n {
        out.push(polyg[k]);
        if let Some(e) = &edges[k] {
            out.extend(e.iter().copied());
        }
    }
    out
}

fn s_curve(amplitude: f64) -> Vec<Pt> {
    let steps = 24;
    (1..steps)
        .map(|k| {
            (
                k as f64 / steps as f64,
                amplitude * (2.0 * PI * k as f64 / steps as f64).sin(),
            )
        })
        .collect()
}

const TAB: [Pt; 11] = [
    (0.36, 0.0),
    (0.40, 0.05),
    (0.36, 0.13),
    (0.37, 0.21),
    (0.43, 0.26),
    (0.50, 0.275),
    (0.57, 0.26),
    (0.63, 0.21),
    (0.64, 0.13),
    (0.60, 0.05),
    (0.64, 0.0),
];

fn hexagon(a: f64) -> Vec<Pt> {
    (0..6)
        .map(|k| {
            let d = (60.0 * k as f64).to_radians();
            (a * d.cos(), a * d.sin())
        })
        .collect()
}

// --------------------------------------------------------------------------- //
// Parameter values
// --------------------------------------------------------------------------- //

struct P<'a>(&'a Map<String, Value>);

impl P<'_> {
    fn f(&self, k: &str) -> f64 {
        self.0.get(k).and_then(Value::as_f64).unwrap_or(0.0)
    }
    fn i(&self, k: &str) -> f64 {
        self.f(k).trunc()
    }
    fn s(&self, k: &str) -> &str {
        self.0.get(k).and_then(Value::as_str).unwrap_or("")
    }
}

fn built(
    name: &str,
    about: String,
    cell: Pt,
    tiles: Vec<(Vec<Pt>, &'static str)>,
    grout: f64,
    dirs: &[(&str, Direction)],
) -> Built {
    Built {
        name: name.into(),
        about,
        cell,
        tiles,
        grout,
        dirs: dirs.iter().map(|(c, d)| (c.to_string(), *d)).collect(),
        notes: vec![],
    }
}

use Direction::{Angle as A, Concentric as Rings};

// --------------------------------------------------------------------------- //
// The patterns
// --------------------------------------------------------------------------- //

fn herringbone(p: &P) -> Result<Built, Error> {
    let (n, gr) = (p.i("plank_ratio"), p.f("grout"));
    if p.s("layout") == "square" {
        let w = p.f("plank_width");
        let h = rect(0.0, 0.0, n * w, w);
        let v = rect(n * w, (1.0 - n) * w, w, n * w);
        let cell = (2.0 * n * w, 2.0 * n * w);
        let tiles = periodic(&[(h, LIGHT), (v, DARK)], [(w, w), (n * w, -n * w)], cell);
        return Ok(built(
            "Herringbone",
            format!(
                "Planks {} x {} mm along x and y, {} mm grout, zig-zag running at 45 deg.",
                g(w),
                g(n * w),
                g(gr)
            ),
            cell,
            tiles,
            gr,
            &[(LIGHT, A(0.0)), (DARK, A(90.0))],
        ));
    }
    let w = p.f("plank_width");
    let (tiles, cell) = herringbone_45(w, n, (0.0, 0.0));
    Ok(built(
        "Herringbone",
        format!(
            "Planks {} x {} mm at 45 deg, {} mm grout, zig-zag along x.",
            g(w),
            g(n * w),
            g(gr)
        ),
        cell,
        tiles,
        gr,
        &[(LIGHT, A(-45.0)), (DARK, A(45.0))],
    ))
}

fn chevron(p: &P) -> Result<Built, Error> {
    let (cw, h, gr) = (p.f("column_width"), p.f("plank_height"), p.f("grout"));
    let left = vec![(0.0, 0.0), (cw, cw), (cw, cw + h), (0.0, h)];
    let right = vec![(cw, cw), (2.0 * cw, 0.0), (2.0 * cw, h), (cw, cw + h)];
    let cell = (2.0 * cw, h);
    let tiles = periodic(
        &[(left, LIGHT), (right, DARK)],
        [(0.0, h), (2.0 * cw, 0.0)],
        cell,
    );
    Ok(built(
        "Chevron",
        format!(
            "Columns {} mm wide, planks {} mm tall with 45 deg ends, {} mm grout.",
            g(cw),
            g(h),
            g(gr)
        ),
        cell,
        tiles,
        gr,
        &[(LIGHT, A(45.0)), (DARK, A(-45.0))],
    ))
}

fn running_bond(p: &P) -> Result<Built, Error> {
    let (bw, bh, gr) = (p.f("brick_length"), p.f("brick_height"), p.f("grout"));
    let k = match p.s("offset") {
        "third" => 3.0,
        "quarter" => 4.0,
        _ => 2.0,
    };
    let cell = (bw, k * bh);
    let tiles = periodic(
        &[(rect(0.0, 0.0, bw, bh), BRICK)],
        [(bw, 0.0), (bw / k, bh)],
        cell,
    );
    Ok(built(
        "Running bond bricks",
        format!(
            "Bricks {} x {} mm, each row shifted half a brick, {} mm grout.",
            g(bw),
            g(bh),
            g(gr)
        ),
        cell,
        tiles,
        gr,
        &[(BRICK, A(0.0))],
    ))
}

fn basketweave(p: &P) -> Result<Built, Error> {
    let (w, gr) = (p.f("plank_width"), p.f("grout"));
    let n = if p.0.contains_key("planks") {
        p.i("planks")
    } else {
        2.0
    };
    let mut base: Vec<(Vec<Pt>, &'static str)> = (0..n as i64)
        .map(|k| (rect(0.0, k as f64 * w, n * w, w), LIGHT))
        .collect();
    base.extend((0..n as i64).map(|k| (rect(n * w + k as f64 * w, 0.0, w, n * w), DARK)));
    let cell = (2.0 * n * w, 2.0 * n * w);
    let tiles = periodic(&base, [(2.0 * n * w, 0.0), (n * w, n * w)], cell);
    let (sets, set) = if n == 2.0 {
        ("pairs".to_string(), "pair")
    } else {
        (format!("sets of {}", n as i64), "set")
    };
    Ok(built(
        "Basketweave",
        format!(
            "Planks {} x {} mm in {}, every other {} turned 90 deg, {} mm grout.",
            g(w),
            g(n * w),
            sets,
            set,
            g(gr)
        ),
        cell,
        tiles,
        gr,
        &[(LIGHT, A(0.0)), (DARK, A(90.0))],
    ))
}

fn parquet_diagonal(p: &P) -> Result<Built, Error> {
    let (s, gr) = (p.f("square"), p.f("grout"));
    let cell = (2.0 * s, 2.0 * s);
    let tiles = periodic(
        &[(rect(0.0, 0.0, s, s), LIGHT), (rect(s, 0.0, s, s), DARK)],
        [(2.0 * s, 0.0), (s, s)],
        cell,
    );
    Ok(built(
        "Diagonal parquet",
        format!(
            "{} mm squares, lines at 45 deg and -45 deg in a checkerboard, {} mm grout.",
            g(s),
            g(gr)
        ),
        cell,
        tiles,
        gr,
        &[(LIGHT, A(45.0)), (DARK, A(-45.0))],
    ))
}

fn honeycomb(p: &P) -> Result<Built, Error> {
    let (a, gr) = (p.f("side"), p.f("grout"));
    let cell = (3.0 * a, R3 * a);
    let tiles = periodic(
        &[(hexagon(a), HONEY)],
        [(1.5 * a, R3 * a / 2.0), (0.0, R3 * a)],
        cell,
    );
    Ok(built(
        "Honeycomb",
        format!(
            "Hexagons with {} mm sides, concentric rings, {} mm grout.",
            g(a),
            g(gr)
        ),
        cell,
        tiles,
        gr,
        &[(HONEY, Rings)],
    ))
}

fn tumbling_blocks(p: &P) -> Result<Built, Error> {
    let (a, gr) = (p.f("side"), p.f("grout"));
    let v: Vec<Pt> = (0..6)
        .map(|k| {
            let d = (30.0 + 60.0 * k as f64).to_radians();
            (a * d.cos(), a * d.sin())
        })
        .collect();
    let o = (0.0, 0.0);
    let top = vec![o, v[0], v[1], v[2]];
    let left = vec![o, v[2], v[3], v[4]];
    let right = vec![o, v[4], v[5], v[0]];
    let cell = (R3 * a, 3.0 * a);
    let tiles = periodic(
        &[(top, BLOCKS[0]), (left, BLOCKS[1]), (right, BLOCKS[2])],
        [(R3 * a, 0.0), (R3 * a / 2.0, 1.5 * a)],
        cell,
    );
    Ok(built(
        "Tumbling blocks",
        format!(
            "Rhombi with {} mm sides in three directions, {} mm grout.",
            g(a),
            g(gr)
        ),
        cell,
        tiles,
        gr,
        &[
            (BLOCKS[0], A(0.0)),
            (BLOCKS[1], A(60.0)),
            (BLOCKS[2], A(-60.0)),
        ],
    ))
}

fn hopscotch(p: &P) -> Result<Built, Error> {
    let (u, gr) = (p.f("unit"), p.f("grout"));
    let (a, b) = (2.0 * u, u);
    let cell = (5.0 * u, 5.0 * u);
    let tiles = periodic(
        &[(rect(0.0, 0.0, a, a), LIGHT), (rect(a, 0.0, b, b), DARK)],
        [(a, b), (-b, a)],
        cell,
    );
    Ok(built("Hopscotch", format!("{} mm and {} mm squares, big squares lined at 45 deg, small ones in rings, {} mm grout.", g(a), g(b), g(gr)),
        cell, tiles, gr, &[(LIGHT, A(45.0)), (DARK, Rings)]))
}

fn stack_bond(p: &P) -> Result<Built, Error> {
    let (l, h, gr) = (p.f("tile_length"), p.f("tile_height"), p.f("grout"));
    Ok(built(
        "Stack bond",
        format!(
            "{} x {} mm tiles in straight rows and columns, {} mm grout.",
            g(l),
            g(h),
            g(gr)
        ),
        (l, h),
        vec![(rect(0.0, 0.0, l, h), CREAM)],
        gr,
        &[(CREAM, A(0.0))],
    ))
}

fn flemish_bond(p: &P) -> Result<Built, Error> {
    let (u, gr) = (p.f("header"), p.f("grout"));
    let cell = (3.0 * u, 2.0 * u);
    let tiles = periodic(
        &[
            (rect(0.0, 0.0, 2.0 * u, u), BRICK),
            (rect(2.0 * u, 0.0, u, u), CLAY),
        ],
        [(3.0 * u, 0.0), (1.5 * u, u)],
        cell,
    );
    Ok(built(
        "Flemish bond",
        format!(
            "Long and short bricks ({} and {} mm) alternating in every row, {} mm grout.",
            g(2.0 * u),
            g(u),
            g(gr)
        ),
        cell,
        tiles,
        gr,
        &[(BRICK, A(0.0)), (CLAY, A(90.0))],
    ))
}

fn english_bond(p: &P) -> Result<Built, Error> {
    let (u, gr) = (p.f("header"), p.f("grout"));
    let cell = (2.0 * u, 2.0 * u);
    let tiles = periodic(
        &[
            (rect(0.0, 0.0, 2.0 * u, u), BRICK),
            (rect(0.5 * u, u, u, u), CLAY),
            (rect(1.5 * u, u, u, u), CLAY),
        ],
        [(2.0 * u, 0.0), (0.0, 2.0 * u)],
        cell,
    );
    Ok(built(
        "English bond",
        format!(
            "Rows of long bricks ({} mm) alternating with rows of short ones, {} mm grout.",
            g(2.0 * u),
            g(gr)
        ),
        cell,
        tiles,
        gr,
        &[(BRICK, A(0.0)), (CLAY, A(90.0))],
    ))
}

fn pinwheel(p: &P) -> Result<Built, Error> {
    let (a, b, gr) = (p.f("rect_length"), p.f("rect_width"), p.f("grout"));
    if a <= b {
        return err("the rectangles must be longer than they are wide");
    }
    let cell = (a + b, a + b);
    let base = vec![
        (rect(0.0, 0.0, a, b), LIGHT),
        (rect(a, 0.0, b, a), DARK),
        (rect(b, a, a, b), LIGHT),
        (rect(0.0, b, b, a), DARK),
        (rect(b, b, a - b, a - b), CREAM),
    ];
    let tiles = periodic(&base, [(a + b, 0.0), (0.0, a + b)], cell);
    Ok(built(
        "Pinwheel",
        format!(
            "Four {} x {} mm rectangles turning round a {} mm square, {} mm grout.",
            g(a),
            g(b),
            g(a - b),
            g(gr)
        ),
        cell,
        tiles,
        gr,
        &[(LIGHT, A(0.0)), (DARK, A(90.0)), (CREAM, Rings)],
    ))
}

fn harlequin(p: &P) -> Result<Built, Error> {
    let (w, h, gr) = (p.f("width"), p.f("height"), p.f("grout"));
    let cell = (w, h);
    let tiles = periodic(
        &[
            (
                vec![(w / 2.0, 0.0), (w, h / 2.0), (w / 2.0, h), (0.0, h / 2.0)],
                AQUA,
            ),
            (
                vec![
                    (0.0, -h / 2.0),
                    (w / 2.0, 0.0),
                    (0.0, h / 2.0),
                    (-w / 2.0, 0.0),
                ],
                TEAL,
            ),
        ],
        [(w, 0.0), (0.0, h)],
        cell,
    );
    let edge: f64 = format!("{:.3}", h.atan2(w).to_degrees()).parse().unwrap();
    Ok(built(
        "Harlequin",
        format!(
            "Diamonds {} mm wide and {} mm tall in alternating colours, {} mm grout.",
            g(w),
            g(h),
            g(gr)
        ),
        cell,
        tiles,
        gr,
        &[(AQUA, A(edge)), (TEAL, A(-edge))],
    ))
}

fn checkerboard(p: &P) -> Result<Built, Error> {
    let (s, gr) = (p.f("square"), p.f("grout"));
    let cell = (2.0 * s, 2.0 * s);
    let tiles = periodic(
        &[(rect(0.0, 0.0, s, s), CREAM), (rect(s, 0.0, s, s), SLATE)],
        [(2.0 * s, 0.0), (s, s)],
        cell,
    );
    Ok(built(
        "Checkerboard",
        format!(
            "{} mm squares, lines across and along in alternate squares, {} mm grout.",
            g(s),
            g(gr)
        ),
        cell,
        tiles,
        gr,
        &[(CREAM, A(0.0)), (SLATE, A(90.0))],
    ))
}

fn octagon_dot(p: &P) -> Result<Built, Error> {
    let (w, gr) = (p.f("octagon"), p.f("grout"));
    let c = w / (2.0 + R2);
    let octagon = vec![
        (c, 0.0),
        (w - c, 0.0),
        (w, c),
        (w, w - c),
        (w - c, w),
        (c, w),
        (0.0, w - c),
        (0.0, c),
    ];
    let dot = vec![(c, 0.0), (0.0, c), (-c, 0.0), (0.0, -c)];
    Ok(built(
        "Octagon and dot",
        format!(
            "{} mm octagons with small square dots between them, {} mm grout.",
            g(w),
            g(gr)
        ),
        (w, w),
        vec![(octagon, CREAM), (dot, NAVY)],
        gr,
        &[(CREAM, Rings), (NAVY, A(45.0))],
    ))
}

fn penny_round(p: &P) -> Result<Built, Error> {
    let (d, gr) = (p.f("diameter"), p.f("grout"));
    let r = d / 2.0;
    let square = p.s("grid") == "square";
    let (base, lattice, cell) = if square {
        let centres = [(0.0, 0.0), (d, 0.0), (d, d), (0.0, d)];
        let discs = geom::union_rings(
            centres
                .iter()
                .map(|&(x, y)| disc(x, y, r))
                .collect::<Vec<_>>()
                .iter(),
        );
        let gap = geom::difference(&mp(poly(&centres)), &discs);
        (
            vec![
                (outline_of(&mp(disc(0.0, 0.0, r)))?, AQUA),
                (outline_of(&gap)?, SLATE),
            ],
            [(d, 0.0), (0.0, d)],
            (d, d),
        )
    } else {
        let h = d * R3 / 2.0;
        let up = [(0.0, 0.0), (d, 0.0), (d / 2.0, h)];
        let down = [(d, 0.0), (1.5 * d, h), (d / 2.0, h)];
        let discs = geom::union_rings(
            up.iter()
                .chain(down.iter())
                .map(|&(x, y)| disc(x, y, r))
                .collect::<Vec<_>>()
                .iter(),
        );
        (
            vec![
                (outline_of(&mp(disc(0.0, 0.0, r)))?, AQUA),
                (
                    outline_of(&geom::difference(&mp(poly(&up)), &discs))?,
                    SLATE,
                ),
                (
                    outline_of(&geom::difference(&mp(poly(&down)), &discs))?,
                    SLATE,
                ),
            ],
            [(d, 0.0), (d / 2.0, h)],
            (d, 2.0 * h),
        )
    };
    let tiles = periodic(&base, lattice, cell);
    Ok(built(
        "Penny round",
        format!(
            "{} mm round tiles{}, {} mm grout.",
            g(d),
            if square { " in a square grid" } else { "" },
            g(gr)
        ),
        cell,
        tiles,
        gr,
        &[(AQUA, Rings), (SLATE, A(0.0))],
    ))
}

fn fish_scale(p: &P) -> Result<Built, Error> {
    let (r, gr) = (p.f("scale_width") / 2.0, p.f("grout"));
    let cut = geom::union_rings([disc(-r, r, r), disc(r, r, r), disc(0.0, 2.0 * r, r)].iter());
    let pts = outline_of(&geom::difference(&mp(disc(0.0, 0.0, r)), &cut))?;
    let cell = (2.0 * r, 2.0 * r);
    let tiles = periodic(
        &[(pts.clone(), AQUA), (mv(&pts, r, r), TEAL)],
        [(2.0 * r, 0.0), (0.0, 2.0 * r)],
        cell,
    );
    Ok(built(
        "Fish scale",
        format!(
            "Overlapping scales {} mm wide, alternate rows in two colours, {} mm grout.",
            g(2.0 * r),
            g(gr)
        ),
        cell,
        tiles,
        gr,
        &[(AQUA, Rings), (TEAL, Rings)],
    ))
}

fn long_hexagon(p: &P) -> Result<Built, Error> {
    let (l, h, gr) = (p.f("flat_length"), p.f("height"), p.f("grout"));
    let q = h / (2.0 * R3);
    let hexa = vec![
        (0.0, h / 2.0),
        (q, 0.0),
        (q + l, 0.0),
        (2.0 * q + l, h / 2.0),
        (q + l, h),
        (q, h),
    ];
    let cell = (2.0 * (q + l), h);
    let tiles = periodic(
        &[(hexa.clone(), SAND), (mv(&hexa, q + l, h / 2.0), CLAY)],
        [(2.0 * (q + l), 0.0), (0.0, h)],
        cell,
    );
    Ok(built(
        "Elongated hexagon",
        format!(
            "Long hexagons ({} mm flat sides, {} mm tall) in staggered rows, {} mm grout.",
            g(l),
            g(h),
            g(gr)
        ),
        cell,
        tiles,
        gr,
        &[(SAND, A(0.0)), (CLAY, A(0.0))],
    ))
}

const FRENCH: [(f64, f64, f64, f64); 8] = [
    (0., 0., 3., 3.),
    (3., 0., 2., 2.),
    (5., 0., 1., 2.),
    (3., 2., 3., 1.),
    (0., 3., 2., 3.),
    (2., 3., 2., 2.),
    (2., 5., 2., 1.),
    (4., 3., 2., 3.),
];

fn french_pattern(p: &P) -> Result<Built, Error> {
    let (u, gr) = (p.f("unit"), p.f("grout"));
    let base: Vec<(Vec<Pt>, &'static str)> = FRENCH
        .iter()
        .map(|&(x, y, w, h)| {
            let col = if w == h {
                CREAM
            } else if w > h {
                SAND
            } else {
                CLAY
            };
            (rect(x * u, y * u, w * u, h * u), col)
        })
        .collect();
    let cell = (6.0 * u, 18.0 * u);
    let tiles = periodic(&base, [(6.0 * u, 0.0), (2.0 * u, 6.0 * u)], cell);
    Ok(built(
        "French pattern",
        format!(
            "Squares and rectangles of four sizes on a {} mm unit, each block offset, {} mm grout.",
            g(u),
            g(gr)
        ),
        cell,
        tiles,
        gr,
        &[(CREAM, A(45.0)), (SAND, A(0.0)), (CLAY, A(90.0))],
    ))
}

fn double_herringbone(p: &P) -> Result<Built, Error> {
    let (w, n, gr) = (p.f("plank_width"), p.i("plank_ratio"), p.f("grout"));
    let (w2, l) = (2.0 * w, 2.0 * n * w);
    let h1 = rot(&rect(0.0, 0.0, l, w), -45.0);
    let h2 = rot(&rect(0.0, w, l, w), -45.0);
    let v1 = rot(&rect(l, (1.0 - n) * w2, w, l), -45.0);
    let v2 = rot(&rect(l + w, (1.0 - n) * w2, w, l), -45.0);
    let cell = (w2 * R2, n * w2 * R2);
    let tiles = periodic(
        &[(h1, LIGHT), (h2, LIGHT), (v1, DARK), (v2, DARK)],
        [(cell.0, 0.0), (0.0, cell.1)],
        cell,
    );
    Ok(built(
        "Double herringbone",
        format!(
            "Pairs of {} x {} mm planks at 45 deg, zig-zag along x, {} mm grout.",
            g(w),
            g(l),
            g(gr)
        ),
        cell,
        tiles,
        gr,
        &[(LIGHT, A(-45.0)), (DARK, A(45.0))],
    ))
}

fn triangles(p: &P) -> Result<Built, Error> {
    let (a, gr) = (p.f("side"), p.f("grout"));
    let h = a * R3 / 2.0;
    let cell = (a, 2.0 * h);
    let tiles = periodic(
        &[
            (vec![(0.0, 0.0), (a, 0.0), (a / 2.0, h)], AQUA),
            (vec![(a, 0.0), (1.5 * a, h), (a / 2.0, h)], NAVY),
        ],
        [(a, 0.0), (a / 2.0, h)],
        cell,
    );
    Ok(built(
        "Triangles",
        format!(
            "Triangles with {} mm sides, pointing up and down in turn, {} mm grout.",
            g(a),
            g(gr)
        ),
        cell,
        tiles,
        gr,
        &[(AQUA, A(60.0)), (NAVY, A(-60.0))],
    ))
}

fn hexagons_triangles(p: &P) -> Result<Built, Error> {
    let (a, gr) = (p.f("side"), p.f("grout"));
    let h = a * R3 / 2.0;
    let cell = (2.0 * a, 2.0 * R3 * a);
    let tiles = periodic(
        &[
            (hexagon(a), SAND),
            (vec![(a, 0.0), (1.5 * a, h), (a / 2.0, h)], TEAL),
            (vec![(a / 2.0, h), (0.0, 2.0 * h), (-a / 2.0, h)], TEAL),
        ],
        [(2.0 * a, 0.0), (a, R3 * a)],
        cell,
    );
    Ok(built(
        "Hexagons and triangles",
        format!(
            "Hexagons and triangles with {} mm sides (trihexagonal), {} mm grout.",
            g(a),
            g(gr)
        ),
        cell,
        tiles,
        gr,
        &[(SAND, Rings), (TEAL, A(0.0))],
    ))
}

fn squares_triangles(p: &P) -> Result<Built, Error> {
    let (a, gr) = (p.f("side"), p.f("grout"));
    let h = a * R3 / 2.0;
    let cell = (a, 2.0 * (a + h));
    let tiles = periodic(
        &[
            (rect(0.0, 0.0, a, a), CREAM),
            (vec![(0.0, a), (a, a), (a / 2.0, a + h)], CLAY),
            (vec![(a, a), (1.5 * a, a + h), (a / 2.0, a + h)], SLATE),
        ],
        [(a, 0.0), (a / 2.0, a + h)],
        cell,
    );
    Ok(built(
        "Squares and triangles",
        format!(
            "Rows of {} mm squares between rows of triangles, {} mm grout.",
            g(a),
            g(gr)
        ),
        cell,
        tiles,
        gr,
        &[(CREAM, A(0.0)), (CLAY, A(60.0)), (SLATE, A(-60.0))],
    ))
}

fn hexagon_flower(p: &P) -> Result<Built, Error> {
    let (a, gr) = (p.f("side"), p.f("grout"));
    let hexa = hexagon(a);
    let e1 = (1.5 * a, R3 * a / 2.0);
    let e2 = (0.0, R3 * a);
    let petals = [
        e1,
        (-e1.0, -e1.1),
        e2,
        (0.0, -e2.1),
        (e2.0 - e1.0, e2.1 - e1.1),
        (e1.0 - e2.0, e1.1 - e2.1),
    ];
    let t1 = (2.0 * e1.0 + e2.0, 2.0 * e1.1 + e2.1);
    let t2 = (-e1.0 + 3.0 * e2.0, -e1.1 + 3.0 * e2.1);
    let mut base = vec![(hexa.clone(), HONEY)];
    base.extend(petals.iter().map(|&(dx, dy)| (mv(&hexa, dx, dy), CREAM)));
    let cell = (21.0 * a, 7.0 * R3 * a);
    let tiles = periodic(&base, [t1, t2], cell);
    Ok(built(
        "Hexagon flower",
        format!(
            "Hexagons with {} mm sides in flowers of seven: a centre and six petals, {} mm grout.",
            g(a),
            g(gr)
        ),
        cell,
        tiles,
        gr,
        &[(HONEY, Rings), (CREAM, A(0.0))],
    ))
}

fn crosses(p: &P) -> Result<Built, Error> {
    let (u, gr) = (p.f("arm"), p.f("grout"));
    let h = u / 2.0;
    let cross = vec![
        (-h, -3.0 * h),
        (h, -3.0 * h),
        (h, -h),
        (3.0 * h, -h),
        (3.0 * h, h),
        (h, h),
        (h, 3.0 * h),
        (-h, 3.0 * h),
        (-h, h),
        (-3.0 * h, h),
        (-3.0 * h, -h),
        (-h, -h),
    ];
    let cell = (5.0 * u, 10.0 * u);
    let tiles = periodic(
        &[(cross.clone(), CREAM), (mv(&cross, 2.0 * u, u), TEAL)],
        [(4.0 * u, 2.0 * u), (-u, 2.0 * u)],
        cell,
    );
    Ok(built(
        "Crosses",
        format!(
            "Interlocking crosses with {} mm arms in two colours, {} mm grout.",
            g(u),
            g(gr)
        ),
        cell,
        tiles,
        gr,
        &[(CREAM, A(45.0)), (TEAL, A(-45.0))],
    ))
}

fn staircase(p: &P) -> Result<Built, Error> {
    let (u, gr) = (p.f("step"), p.f("grout"));
    let ell = vec![
        (0.0, 0.0),
        (2.0 * u, 0.0),
        (2.0 * u, u),
        (u, u),
        (u, 2.0 * u),
        (0.0, 2.0 * u),
    ];
    let cell = (6.0 * u, 6.0 * u);
    let tiles = periodic(
        &[(ell.clone(), SAND), (mv(&ell, u, u), SLATE)],
        [(2.0 * u, 2.0 * u), (3.0 * u, 0.0)],
        cell,
    );
    Ok(built(
        "Staircase",
        format!(
            "L-shaped tiles ({} mm steps) climbing in a diagonal, {} mm grout.",
            g(u),
            g(gr)
        ),
        cell,
        tiles,
        gr,
        &[(SAND, A(0.0)), (SLATE, A(90.0))],
    ))
}

fn arrows(p: &P) -> Result<Built, Error> {
    let (w, t, angle, gr) = (
        p.f("arrow_width") / 2.0,
        p.f("thickness"),
        p.f("slope"),
        p.f("grout"),
    );
    let rise = w * angle.to_radians().tan();
    let arrow = vec![
        (0.0, 0.0),
        (w, rise),
        (2.0 * w, 0.0),
        (2.0 * w, t),
        (w, rise + t),
        (0.0, t),
    ];
    let cell = (2.0 * w, 2.0 * t);
    let tiles = periodic(
        &[(arrow.clone(), SAND), (mv(&arrow, 0.0, t), NAVY)],
        [(2.0 * w, 0.0), (0.0, 2.0 * t)],
        cell,
    );
    Ok(built(
        "Arrows",
        format!(
            "Stacked chevron tiles {} mm wide and {} mm thick, {} mm grout.",
            g(2.0 * w),
            g(t),
            g(gr)
        ),
        cell,
        tiles,
        gr,
        &[(SAND, Rings), (NAVY, Rings)],
    ))
}

fn star_and_cross(p: &P) -> Result<Built, Error> {
    let (d, gr) = (p.f("spacing"), p.f("grout"));
    let r = d / 2.0;
    let star = |cx: f64, cy: f64| -> MP {
        let sq = poly(&[(cx + r, cy), (cx, cy + r), (cx - r, cy), (cx, cy - r)]);
        let k = r / R2;
        let sq2 = poly(&[
            (cx + k, cy + k),
            (cx - k, cy + k),
            (cx - k, cy - k),
            (cx + k, cy - k),
        ]);
        geom::union_rings([sq, sq2].iter())
    };
    let stars: Vec<Polygon<f64>> = [(0.0, 0.0), (0.0, d), (d, 0.0), (d, d)]
        .iter()
        .flat_map(|&(x, y)| star(x, y).0)
        .collect();
    let filler = geom::difference(
        &mp(poly(&rect(0.0, 0.0, d, d))),
        &geom::union_rings(stars.iter()),
    );
    Ok(built(
        "Star and cross",
        format!(
            "Eight-point stars {} mm across with crosses between them, {} mm grout.",
            g(d),
            g(gr)
        ),
        (d, d),
        vec![
            (outline_of(&star(0.0, 0.0))?, CREAM),
            (outline_of(&filler)?, TEAL),
        ],
        gr,
        &[(CREAM, Rings), (TEAL, A(45.0))],
    ))
}

fn ogee(p: &P) -> Result<Built, Error> {
    let (w, h, depth, gr) = (
        p.f("width"),
        p.f("height"),
        p.f("curve") / 100.0,
        p.f("grout"),
    );
    let diamond = [(w / 2.0, 0.0), (w, h / 2.0), (w / 2.0, h), (0.0, h / 2.0)];
    let tile = bend(&diamond, &[(0, 2, s_curve(depth)), (1, 3, s_curve(-depth))]);
    let cell = (w, h);
    let tiles = periodic(
        &[(tile.clone(), AQUA), (mv(&tile, -w / 2.0, -h / 2.0), CREAM)],
        [(w, 0.0), (0.0, h)],
        cell,
    );
    Ok(built(
        "Ogee",
        format!(
            "Onion-dome tiles {} mm wide and {} mm tall in two colours, {} mm grout.",
            g(w),
            g(h),
            g(gr)
        ),
        cell,
        tiles,
        gr,
        &[(AQUA, Rings), (CREAM, Rings)],
    ))
}

fn lantern(p: &P) -> Result<Built, Error> {
    let (w, h, depth, gr) = (
        p.f("width"),
        p.f("height"),
        p.f("curve") / 100.0,
        p.f("grout"),
    );
    let (tip, side) = (h / 2.0, h / 4.0);
    let hexa = [
        (0.0, -tip),
        (w / 2.0, -side),
        (w / 2.0, side),
        (0.0, tip),
        (-w / 2.0, side),
        (-w / 2.0, -side),
    ];
    let tile = bend(&hexa, &[(0, 3, s_curve(depth)), (2, 5, s_curve(-depth))]);
    let cell = (w, 2.0 * (tip + side));
    let tiles = periodic(
        &[
            (tile.clone(), CREAM),
            (mv(&tile, w / 2.0, tip + side), TEAL),
        ],
        [(w, 0.0), (0.0, 2.0 * (tip + side))],
        cell,
    );
    Ok(built(
        "Lantern",
        format!(
            "Moroccan lantern (arabesque) tiles {} x {} mm in two colours, {} mm grout.",
            g(w),
            g(h),
            g(gr)
        ),
        cell,
        tiles,
        gr,
        &[(CREAM, Rings), (TEAL, Rings)],
    ))
}

fn jigsaw(p: &P) -> Result<Built, Error> {
    let (s, gr) = (p.f("piece"), p.f("grout"));
    let square = [(0.0, 0.0), (s, 0.0), (s, s), (0.0, s)];
    let tile = bend(
        &square,
        &[
            (0, 2, TAB.to_vec()),
            (1, 3, TAB.iter().map(|&(u, v)| (u, -v)).collect()),
        ],
    );
    let cell = (2.0 * s, 2.0 * s);
    let tiles = periodic(
        &[(tile.clone(), SAND), (mv(&tile, s, 0.0), SLATE)],
        [(2.0 * s, 0.0), (s, s)],
        cell,
    );
    Ok(built(
        "Jigsaw",
        format!(
            "Interlocking puzzle pieces {} mm across, {} mm grout.",
            g(s),
            g(gr)
        ),
        cell,
        tiles,
        gr,
        &[(SAND, A(0.0)), (SLATE, A(90.0))],
    ))
}

fn waves(p: &P) -> Result<Built, Error> {
    let (w, h, depth, gr) = (
        p.f("wavelength"),
        p.f("band"),
        p.f("curve") / 100.0,
        p.f("grout"),
    );
    let band = [(0.0, 0.0), (w, 0.0), (w, h), (0.0, h)];
    let tile = bend(&band, &[(0, 2, s_curve(depth))]);
    let cell = (w, 2.0 * h);
    let tiles = periodic(
        &[(tile.clone(), AQUA), (mv(&tile, 0.0, h), NAVY)],
        [(w, 0.0), (0.0, 2.0 * h)],
        cell,
    );
    Ok(built(
        "Waves",
        format!(
            "Wavy bands {} mm tall, one wave every {} mm, {} mm grout.",
            g(h),
            g(w),
            g(gr)
        ),
        cell,
        tiles,
        gr,
        &[(AQUA, A(0.0)), (NAVY, A(0.0))],
    ))
}

fn soldier_course(p: &P) -> Result<Built, Error> {
    let (hgt, n, gr) = (p.f("brick_height"), p.i("rows"), p.f("grout"));
    let l = 2.0 * hgt;
    let mut base: Vec<(Vec<Pt>, &'static str)> = (0..n as i64)
        .map(|k| {
            let x = (k % 2) as f64 * l / 2.0;
            (rect(x, k as f64 * hgt, l, hgt), BRICK)
        })
        .collect();
    base.push((rect(0.0, n * hgt, hgt, l), CLAY));
    base.push((rect(hgt, n * hgt, hgt, l), CLAY));
    let cell = (l, n * hgt + l);
    let tiles = periodic(&base, [(l, 0.0), (0.0, n * hgt + l)], cell);
    Ok(built(
        "Soldier course",
        format!(
            "{} rows of running bond bricks ({} x {} mm) then a row standing up, {} mm grout.",
            n as i64,
            g(l),
            g(hgt),
            g(gr)
        ),
        cell,
        tiles,
        gr,
        &[(BRICK, A(0.0)), (CLAY, A(90.0))],
    ))
}

fn hexagons_squares(p: &P) -> Result<Built, Error> {
    let (a, gr) = (p.f("side"), p.f("grout"));
    let hexa = hexagon(a);
    let normal = |deg: f64| {
        let (s, c) = deg.to_radians().sin_cos();
        (c, s)
    };
    let mut base = vec![(hexa.clone(), SAND)];
    for (k, deg) in [(0usize, 30.0), (1, 90.0), (2, 150.0)] {
        let ((x0, y0), (x1, y1)) = (hexa[k], hexa[k + 1]);
        let (nx, ny) = normal(deg);
        base.push((
            vec![
                (x0, y0),
                (x0 + a * nx, y0 + a * ny),
                (x1 + a * nx, y1 + a * ny),
                (x1, y1),
            ],
            TEAL,
        ));
    }
    for (k, (d0, d1)) in [(0usize, (-30.0, 30.0)), (1, (30.0, 90.0))] {
        let (x, y) = hexa[k];
        base.push((
            vec![
                (x, y),
                (x + a * normal(d0).0, y + a * normal(d0).1),
                (x + a * normal(d1).0, y + a * normal(d1).1),
            ],
            CREAM,
        ));
    }
    let dd = a * (1.0 + R3);
    let cell = (R3 * dd, dd);
    let tiles = periodic(&base, [(dd * R3 / 2.0, dd / 2.0), (0.0, dd)], cell);
    Ok(built("Hexagons, squares and triangles", format!("Hexagons ringed by squares and triangles, {} mm sides (rhombitrihexagonal), {} mm grout.", g(a), g(gr)),
        cell, tiles, gr, &[(SAND, Rings), (TEAL, A(0.0)), (CREAM, A(0.0))]))
}

fn dodecagons(p: &P) -> Result<Built, Error> {
    let (a, gr) = (p.f("side"), p.f("grout"));
    let r = a / (2.0 * 15f64.to_radians().sin());
    let apothem = a / (2.0 * 15f64.to_radians().tan());
    let d = 2.0 * apothem;
    let dodecagon = |cx: f64, cy: f64| -> Vec<Pt> {
        (0..12)
            .map(|k| {
                let t = (15.0 + 30.0 * k as f64).to_radians();
                (cx + r * t.cos(), cy + r * t.sin())
            })
            .collect()
    };
    let h = d * R3 / 2.0;
    let up = [(0.0, 0.0), (d, 0.0), (d / 2.0, h)];
    let down = [(d, 0.0), (1.5 * d, h), (d / 2.0, h)];
    let shapes = geom::union_rings(
        up.iter()
            .chain(down.iter())
            .map(|&(x, y)| poly(&dodecagon(x, y)))
            .collect::<Vec<_>>()
            .iter(),
    );
    let base = vec![
        (dodecagon(0.0, 0.0), CREAM),
        (
            outline_of(&geom::difference(&mp(poly(&up)), &shapes))?,
            CLAY,
        ),
        (
            outline_of(&geom::difference(&mp(poly(&down)), &shapes))?,
            CLAY,
        ),
    ];
    let cell = (d, 2.0 * h);
    let tiles = periodic(&base, [(d, 0.0), (d / 2.0, h)], cell);
    Ok(built(
        "Dodecagons and triangles",
        format!(
            "Twelve-sided tiles with {} mm sides and small triangles between, {} mm grout.",
            g(a),
            g(gr)
        ),
        cell,
        tiles,
        gr,
        &[(CREAM, Rings), (CLAY, A(0.0))],
    ))
}

fn floorboards(p: &P) -> Result<Built, Error> {
    let (b, pr, gr) = (p.f("board_width"), p.f("run"), p.f("grout"));
    let rows = 6;
    let mut base = Vec::new();
    for k in 0..rows {
        let o = ((k as f64 * 0.382) % 1.0) * pr;
        let cut = 0.55 * pr;
        let col = [SAND, LIGHT, MID][k % 3];
        base.push((rect(o, k as f64 * b, cut, b), col));
        base.push((rect(o + cut, k as f64 * b, pr - cut, b), col));
    }
    let cell = (pr, rows as f64 * b);
    let tiles = periodic(&base, [(pr, 0.0), (0.0, rows as f64 * b)], cell);
    Ok(built(
        "Floorboards",
        format!(
            "{} mm boards in staggered lengths (the joints repeat every {} mm), {} mm grout.",
            g(b),
            g(pr),
            g(gr)
        ),
        cell,
        tiles,
        gr,
        &[(SAND, A(0.0)), (LIGHT, A(0.0)), (MID, A(0.0))],
    ))
}

fn scatter(key: &str) -> Vec<usize> {
    registry().scatters[key].clone()
}

fn mosaic(p: &P) -> Result<Built, Error> {
    let (s, gr) = (p.f("square"), p.f("grout"));
    let n = 6;
    let cols = [AQUA, TEAL, CREAM];
    let pick = scatter("36_3_11");
    let mut base = Vec::new();
    for i in 0..n {
        for j in 0..n {
            base.push((
                rect(i as f64 * s, j as f64 * s, s, s),
                cols[pick[i * n + j]],
            ));
        }
    }
    let cell = (n as f64 * s, n as f64 * s);
    Ok(built(
        "Mosaic",
        format!(
            "{} mm squares in three colours, scattered, {} mm grout.",
            g(s),
            g(gr)
        ),
        cell,
        base,
        gr,
        &[(AQUA, A(0.0)), (TEAL, A(90.0)), (CREAM, A(45.0))],
    ))
}

fn hexagon_mosaic(p: &P) -> Result<Built, Error> {
    let (a, gr) = (p.f("side"), p.f("grout"));
    let hexa = hexagon(a);
    let (nx, ny) = (3, 4);
    let cols = [AQUA, NAVY, CREAM];
    let mut centres = Vec::new();
    for i in 0..nx {
        for j in 0..ny {
            for (dx, dy) in [(0.0, 0.0), (1.5 * a, R3 * a / 2.0)] {
                centres.push((i as f64 * 3.0 * a + dx, j as f64 * R3 * a + dy));
            }
        }
    }
    let pick = scatter("24_3_5");
    let base: Vec<(Vec<Pt>, &'static str)> = centres
        .iter()
        .zip(&pick)
        .map(|(&(x, y), &k)| (mv(&hexa, x, y), cols[k]))
        .collect();
    let cell = (nx as f64 * 3.0 * a, ny as f64 * R3 * a);
    let tiles = periodic(&base, [(cell.0, 0.0), (0.0, cell.1)], cell);
    Ok(built(
        "Hexagon mosaic",
        format!(
            "Hexagons with {} mm sides in three colours, scattered, {} mm grout.",
            g(a),
            g(gr)
        ),
        cell,
        tiles,
        gr,
        &[(AQUA, Rings), (NAVY, Rings), (CREAM, Rings)],
    ))
}

fn garden_lattice(p: &P) -> Result<Built, Error> {
    let (l, u, gr) = (p.f("square"), p.f("bar"), p.f("grout"));
    let base = vec![
        (rect(0.0, 0.0, l, l), CREAM),
        (rect(0.0, l, l, u), SAND),
        (rect(l, 0.0, u, l), CLAY),
        (rect(l, l, u, u), NAVY),
    ];
    Ok(built(
        "Garden lattice",
        format!(
            "{} mm squares framed by {} mm bars with small squares at the crossings, {} mm grout.",
            g(l),
            g(u),
            g(gr)
        ),
        (l + u, l + u),
        base,
        gr,
        &[
            (CREAM, Rings),
            (SAND, A(0.0)),
            (CLAY, A(90.0)),
            (NAVY, A(45.0)),
        ],
    ))
}

type Build = fn(&P) -> Result<Built, Error>;

fn build_fn(id: &str) -> Option<Build> {
    Some(match id {
        "herringbone" => herringbone,
        "chevron" => chevron,
        "running_bond" => running_bond,
        "basketweave" => basketweave,
        "parquet_diagonal" => parquet_diagonal,
        "honeycomb" => honeycomb,
        "tumbling_blocks" => tumbling_blocks,
        "hopscotch" => hopscotch,
        "stack_bond" => stack_bond,
        "flemish_bond" => flemish_bond,
        "english_bond" => english_bond,
        "pinwheel" => pinwheel,
        "harlequin" => harlequin,
        "checkerboard" => checkerboard,
        "octagon_dot" => octagon_dot,
        "penny_round" => penny_round,
        "fish_scale" => fish_scale,
        "long_hexagon" => long_hexagon,
        "french_pattern" => french_pattern,
        "double_herringbone" => double_herringbone,
        "triangles" => triangles,
        "hexagons_triangles" => hexagons_triangles,
        "squares_triangles" => squares_triangles,
        "hexagon_flower" => hexagon_flower,
        "crosses" => crosses,
        "staircase" => staircase,
        "arrows" => arrows,
        "star_and_cross" => star_and_cross,
        "ogee" => ogee,
        "lantern" => lantern,
        "jigsaw" => jigsaw,
        "waves" => waves,
        "soldier_course" => soldier_course,
        "hexagons_squares" => hexagons_squares,
        "dodecagons" => dodecagons,
        "floorboards" => floorboards,
        "mosaic" => mosaic,
        "hexagon_mosaic" => hexagon_mosaic,
        "garden_lattice" => garden_lattice,
        _ => return None,
    })
}

// --------------------------------------------------------------------------- //
// The registry (library.json)
// --------------------------------------------------------------------------- //

pub struct Spec {
    pub id: String,
    pub name: String,
    pub groups: Vec<Value>,
    pub form: Value,
    /// (id, name, values that differ from the defaults, directions).
    pub presets: Vec<(String, String, Map<String, Value>, Map<String, Value>)>,
}

pub struct Registry {
    pub patterns: Vec<Spec>,
    scatters: std::collections::HashMap<String, Vec<usize>>,
    /// Generator families of version-1 files -> (pattern, extra values).
    pub old_families: Vec<(String, String, Map<String, Value>)>,
}

pub fn registry() -> &'static Registry {
    static REG: OnceLock<Registry> = OnceLock::new();
    REG.get_or_init(|| {
        let v: Value = serde_json::from_str(include_str!("library.json")).expect("library.json");
        let patterns = v["patterns"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| Spec {
                id: p["id"].as_str().unwrap().into(),
                name: p["name"].as_str().unwrap().into(),
                groups: p["groups"].as_array().unwrap().clone(),
                form: p["form"].clone(),
                presets: p["presets"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|q| {
                        (
                            q["id"].as_str().unwrap().into(),
                            q["name"].as_str().unwrap().into(),
                            q["values"].as_object().unwrap().clone(),
                            q["directions"].as_object().unwrap().clone(),
                        )
                    })
                    .collect(),
            })
            .collect();
        let scatters = v["scatters"]
            .as_object()
            .unwrap()
            .iter()
            .map(|(k, a)| {
                (
                    k.clone(),
                    a.as_array()
                        .unwrap()
                        .iter()
                        .map(|x| x.as_u64().unwrap() as usize)
                        .collect(),
                )
            })
            .collect();
        let old_families = v["old_families"]
            .as_object()
            .unwrap()
            .iter()
            .map(|(k, a)| {
                (
                    k.clone(),
                    a[0].as_str().unwrap().into(),
                    a[1].as_object().unwrap().clone(),
                )
            })
            .collect();
        Registry {
            patterns,
            scatters,
            old_families,
        }
    })
}

pub fn spec(id: &str) -> Result<&'static Spec, Error> {
    registry()
        .patterns
        .iter()
        .find(|p| p.id == id)
        .ok_or_else(|| Error(format!("unknown pattern '{id}'")))
}

/// Values for every parameter of a provided pattern (defaults where not given), checked.
pub fn check_values(id: &str, values: Option<&Value>) -> Result<Map<String, Value>, Error> {
    let s = spec(id)?;
    form::check(&s.form, values, &s.name).map_err(|e| Error(e.0))
}

/// Line directions per tile group, checked against the pattern's groups.
pub fn check_directions(
    id: &str,
    directions: Option<&Value>,
) -> Result<Vec<(String, Direction)>, Error> {
    let s = spec(id)?;
    let mut out = Vec::new();
    let Some(Value::Object(m)) = directions else {
        return Ok(out);
    };
    for (k, v) in m {
        if !s.groups.iter().any(|g| g["group"].as_str() == Some(k)) {
            return err(format!("the {id} pattern has no tile group {k}"));
        }
        match (v, Direction::from_json(v)) {
            (Value::Bool(_), _) | (_, None) => return err("a direction is degrees or \"rings\""),
            (_, Some(d)) => out.push((k.clone(), d)),
        }
    }
    Ok(out)
}

/// Build one repeat at checked values.
pub fn build(id: &str, values: &Map<String, Value>) -> Result<Built, Error> {
    let f = build_fn(id).ok_or_else(|| Error(format!("unknown pattern '{id}'")))?;
    f(&P(values))
}

pub fn default_directions(
    id: &str,
    values: Option<&Value>,
) -> Result<Vec<(String, Direction)>, Error> {
    Ok(build(id, &check_values(id, values)?)?.dirs)
}

/// The provided patterns for the app: id, name, form, tile groups (with
/// default directions) and presets (default first), as JSON.
pub fn provided() -> Result<Vec<Value>, Error> {
    let mut out = Vec::new();
    for s in &registry().patterns {
        let defaults = form::defaults(&s.form);
        let dirs = default_directions(&s.id, None)?;
        let mut presets = vec![
            serde_json::json!({"id": "default", "name": "Default", "provided": true,
            "values": defaults, "directions": {}}),
        ];
        for (pid, name, values, directions) in &s.presets {
            let mut merged = defaults.clone();
            merged.extend(values.clone());
            presets.push(serde_json::json!({"id": pid, "name": name, "provided": true,
                "values": check_values(&s.id, Some(&Value::Object(merged)))?, "directions": directions}));
        }
        let groups: Vec<Value> = s
            .groups
            .iter()
            .map(|g| {
                let mut g = g.clone();
                let d = dirs
                    .iter()
                    .find(|(c, _)| Some(c.as_str()) == g["group"].as_str())
                    .map(|(_, d)| d.to_json());
                g["direction"] = d.unwrap_or(Value::Null);
                g
            })
            .collect();
        out.push(
            serde_json::json!({"id": s.id, "name": s.name, "provided": true, "parametric": true,
            "form": s.form, "groups": groups, "presets": presets}),
        );
    }
    Ok(out)
}

/// The SVG of one repeat (`write` in pattern_library.py).
pub fn svg(
    b: &Built,
    title: &str,
    notes: &[String],
    meta: Option<&Value>,
) -> Result<String, Error> {
    let (cw, ch) = b.cell;
    let fy = |y: f64| ch - y;
    let pts_s = |pts: &[Pt]| {
        pts.iter()
            .map(|&(x, y)| format!("{:.4},{:.4}", x, fy(y)))
            .collect::<Vec<_>>()
            .join(" ")
    };
    let mut marks = Vec::new();
    for (col, how) in &b.dirs {
        let cands: Vec<&Vec<Pt>> = b
            .tiles
            .iter()
            .filter(|(_, c)| c == col)
            .map(|(p, _)| p)
            .collect();
        if cands.is_empty() {
            continue;
        }
        // the copy nearest the middle of the page, so the mark is easy to find
        let dist = |q: &Vec<Pt>| {
            let (x, y) = centroid(q);
            (x - cw / 2.0).hypot(y - ch / 2.0)
        };
        let mut p = cands[0];
        for q in &cands[1..] {
            if dist(q) < dist(p) {
                p = q;
            }
        }
        let (cx, cy) = centroid(p);
        match how {
            Direction::Concentric => marks.push(format!("  <circle cx=\"{:.4}\" cy=\"{:.4}\" r=\"{:.2}\"/>   <!-- {} tiles: concentric rings -->", cx, fy(cy), 1.2, col)),
            Direction::Angle(deg) => {
                let (a, bb) = chord(p, cx, cy, *deg, 1.0);
                let a2 = (a.0 * 0.85 + bb.0 * 0.15, a.1 * 0.85 + bb.1 * 0.15);
                let b2 = (a.0 * 0.15 + bb.0 * 0.85, a.1 * 0.15 + bb.1 * 0.85);
                marks.push(format!("  <line x1=\"{:.4}\" y1=\"{:.4}\" x2=\"{:.4}\" y2=\"{:.4}\"/>   <!-- {} tiles: lines at {} deg -->",
                    a2.0, fy(a2.1), b2.0, fy(b2.1), col, g(*deg)));
            }
        }
    }
    if [title, &b.about]
        .iter()
        .chain(notes.iter().map(|n| n.as_str()).collect::<Vec<_>>().iter())
        .any(|s| s.contains("--"))
    {
        return err("no -- in XML comments");
    }
    let mut lines = vec![
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>".to_string(),
        format!("<!-- {title} pattern for TessaFab (made by pattern_library.py)"),
        format!("     {}", b.about),
    ];
    lines.extend(notes.iter().map(|n| format!("     {n}")));
    lines.push(format!("     The page is one repeat ({} x {} mm): the pattern is copied every page width across and", fmt_gp(cw, 4), fmt_gp(ch, 4)));
    lines.push("     every page height up, so tiles hanging over the page edge are completed by the next copy.".into());
    lines.push("     fill = tile; blue stroke = grout (width = groove in mm); light red = line direction per colour,".into());
    lines.push("     closed red loop = concentric rings. No black outlines: those come from the model's layout. -->".into());
    lines.push("<svg xmlns=\"http://www.w3.org/2000/svg\" xmlns:inkscape=\"http://www.inkscape.org/namespaces/inkscape\"".into());
    let meta_attr = match meta {
        None => String::new(),
        Some(m) => format!("\n     data-pattern=\"{}\"", html_escape(&py_json(m))),
    };
    lines.push(format!(
        "     width=\"{cw:.4}mm\" height=\"{ch:.4}mm\" viewBox=\"0 0 {cw:.4} {ch:.4}\"{meta_attr}>"
    ));
    lines.push(format!("<title>{title}</title>"));
    lines.push(format!("<g inkscape:groupmode=\"layer\" inkscape:label=\"tiles\" stroke=\"{BLUE}\" stroke-width=\"{}\" stroke-linejoin=\"miter\">", g(b.grout)));
    for (p, col) in &b.tiles {
        lines.push(format!(
            "  <polygon fill=\"{col}\" points=\"{}\"/>",
            pts_s(p)
        ));
    }
    lines.push("</g>".into());
    lines.push(format!("<g inkscape:groupmode=\"layer\" inkscape:label=\"directions\" fill=\"none\" stroke=\"{RED}\" stroke-width=\"0.5\">"));
    lines.extend(marks);
    lines.push("</g>".into());
    lines.push("</svg>".into());
    Ok(lines.join("\n") + "\n")
}

/// One repeat of a provided pattern at `values` (defaults where not given), with
/// line directions per tile group (the pattern's own where not given): the SVG
/// text and its metadata.
pub fn generate(
    id: &str,
    values: Option<&Value>,
    directions: Option<&Value>,
    title: Option<&str>,
    notes: Option<&[String]>,
) -> Result<(String, Value), Error> {
    let values = check_values(id, values)?;
    let directions = check_directions(id, directions)?;
    let mut b = build(id, &values)?;
    for (c, d) in &directions {
        match b.dirs.iter_mut().find(|(k, _)| k == c) {
            Some(e) => e.1 = *d,
            None => b.dirs.push((c.clone(), *d)),
        }
    }
    let mut meta = serde_json::json!({"id": id, "values": values});
    if !directions.is_empty() {
        meta["directions"] = Value::Object(
            directions
                .iter()
                .map(|(c, d)| (c.clone(), d.to_json()))
                .collect(),
        );
    }
    let own = b.notes.clone();
    let text = svg(
        &b,
        title.unwrap_or(&b.name.clone()),
        notes.unwrap_or(&own),
        Some(&meta),
    )?;
    Ok((text, meta))
}

pub fn preset_file_name(id: &str, preset: &str) -> String {
    if preset == "default" {
        format!("{id}.svg")
    } else {
        format!("{id}_{preset}.svg")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn python_number_formats() {
        assert_eq!(py_float(6.0), "6.0");
        assert_eq!(py_float(0.4), "0.4");
        assert_eq!(py_float(25.4), "25.4");
        assert_eq!(py_float(1e-5), "1e-05");
        assert_eq!(py_float(0.0001), "0.0001");
        assert_eq!(py_float(1.5e16), "1.5e+16");
        assert_eq!(py_float(123456789012345.0), "123456789012345.0");
        assert_eq!(py_float(-2.5), "-2.5");
        assert_eq!(fmt_gp(8.4667, 4), "8.467");
        assert_eq!(fmt_gp(25.4, 4), "25.4");
        assert_eq!(fmt_gp(120.0, 4), "120");
        assert_eq!(
            py_json(&serde_json::json!({"values": {"b": 1, "a": 0.5, "s": "x"}, "id": "h"})),
            "{\"id\": \"h\", \"values\": {\"a\": 0.5, \"b\": 1, \"s\": \"x\"}}"
        );
    }

    #[test]
    fn every_pattern_builds_at_its_defaults() {
        for s in &registry().patterns {
            let (text, _) = generate(&s.id, None, None, None, None)
                .unwrap_or_else(|e| panic!("{}: {}", s.id, e.0));
            assert!(text.contains("<polygon"), "{}", s.id);
        }
        assert_eq!(registry().patterns.len(), 39);
    }
}
