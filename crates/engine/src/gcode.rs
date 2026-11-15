//! G-code reading: printer state, top-surface blocks and the layer's other extrusions.
//! A port of `find_blocks` / `Machine` / `arc_points` in postprocess.py.

use std::collections::HashMap;

pub const TOP_FEATURES: [&str; 2] = ["top surface", "top solid infill"];
/// The face a print lies on: "Bottom surface" (Bambu Studio, OrcaSlicer), or
/// "Solid infill" on the first layer (PrusaSlicer).
pub const BOTTOM_FEATURES: [&str; 2] = ["bottom surface", "solid infill"];
/// Solid layers just under a top surface (redrawn as tiles for colour depth > 1).
pub const SOLID_FEATURES: [&str; 2] = ["internal solid infill", "solid infill"];
const ABORT_CMDS: [&str; 6] = ["G91", "G92", "G28", "M620", "M621", "G29"];

/// Printer state: (x, y, z, e, f).
pub type Snapshot = (f64, f64, f64, f64, f64);

/// One extruded segment: x0, y0, x1, y1, width, e (share of the move's extrusion).
#[derive(Clone, Copy, Debug)]
pub struct Seg {
    pub x0: f64,
    pub y0: f64,
    pub x1: f64,
    pub y1: f64,
    pub w: f64,
    pub e: f64,
    /// Printed as a part's outermost wall (other extrusions only).
    pub outer: bool,
}

#[derive(Default, Debug)]
pub struct Block {
    pub first: Option<usize>,
    pub last: usize,
    pub start: Snapshot,
    pub end: Snapshot,
    pub abs_e: bool,
    pub segments: Vec<Seg>,
    pub ext_len: f64,
    pub ext_e: f64,
    /// feedrate -> extruded length, in first-seen order
    pub f_weight: Vec<(f64, f64)>,
    pub keep: Vec<(usize, String)>,
    pub width: f64,
    pub height: Option<f64>,
    pub abort: Option<String>,
    pub layer: usize,
    pub accel: Option<String>,
}

/// Parameters of one G-code line: letter -> value (None for a bare letter).
pub type Params = Vec<(char, Option<f64>)>;

fn get(p: &Params, k: char) -> Option<f64> {
    p.iter().rev().find(|(c, _)| *c == k).and_then(|(_, v)| *v)
}

fn has(p: &Params, k: char) -> bool {
    p.iter().any(|(c, _)| *c == k)
}

pub fn parse_line(line: &str) -> Option<(String, Params)> {
    let code = line.split(';').next().unwrap_or("").trim();
    if code.is_empty() {
        return None;
    }
    let mut parts = code.split_whitespace();
    let cmd = parts.next()?.to_ascii_uppercase();
    let mut params = Vec::new();
    for p in parts {
        let mut ch = p.chars();
        let k = ch.next().unwrap().to_ascii_uppercase();
        let rest = ch.as_str();
        let v = if rest.is_empty() {
            None
        } else {
            rest.parse::<f64>().ok()
        };
        params.push((k, v));
    }
    Some((cmd, params))
}

/// Points along a G2/G3 arc (start point excluded), like arc_points().
pub fn arc_points(
    x0: f64,
    y0: f64,
    x1: f64,
    y1: f64,
    i: f64,
    j: f64,
    cw: bool,
    max_seg: f64,
) -> Vec<(f64, f64)> {
    let (cx, cy) = (x0 + i, y0 + j);
    let r = (x0 - cx).hypot(y0 - cy);
    let a0 = (y0 - cy).atan2(x0 - cx);
    let a1 = (y1 - cy).atan2(x1 - cx);
    let mut sweep = a1 - a0;
    let full = (x1 - x0).hypot(y1 - y0) < 1e-6;
    let tau = 2.0 * std::f64::consts::PI;
    if cw {
        if sweep >= 0.0 || full {
            sweep -= tau;
        }
    } else if sweep <= 0.0 || full {
        sweep += tau;
    }
    let n = ((sweep.abs() * r / max_seg).ceil() as usize).max(1);
    let mut pts: Vec<(f64, f64)> = (1..=n)
        .map(|k| {
            let a = a0 + sweep * k as f64 / n as f64;
            (cx + r * a.cos(), cy + r * a.sin())
        })
        .collect();
    *pts.last_mut().unwrap() = (x1, y1);
    pts
}

pub struct Move {
    pub cmd: String,
    pub x0: f64,
    pub y0: f64,
    pub z0: f64,
    pub x1: f64,
    pub y1: f64,
    pub z1: f64,
    pub de: f64,
    pub f: f64,
    pub xy: bool,
    pub i: f64,
    pub j: f64,
    pub turns: Option<f64>,
}

impl Move {
    /// A G2/G3 with extra full turns (`P` with a non-zero count), which isn't followed as an arc.
    pub fn multi_turn(&self) -> bool {
        self.turns.is_some_and(|t| t != 0.0)
    }
}

pub struct Machine {
    pub x: f64,
    pub y: f64,
    pub z: f64,
    pub e: f64,
    pub f: f64,
    pub abs_xyz: bool,
    pub abs_e: bool,
}

impl Machine {
    pub fn new() -> Self {
        Machine {
            x: 0.0,
            y: 0.0,
            z: 0.0,
            e: 0.0,
            f: 0.0,
            abs_xyz: true,
            abs_e: true,
        }
    }

    pub fn snapshot(&self) -> Snapshot {
        (self.x, self.y, self.z, self.e, self.f)
    }

    pub fn apply(&mut self, cmd: &str, p: &Params) -> Option<Move> {
        match cmd {
            "G90" => self.abs_xyz = true,
            "G91" => self.abs_xyz = false,
            "M82" => self.abs_e = true,
            "M83" => self.abs_e = false,
            "G92" => {
                if let Some(v) = get(p, 'X') {
                    self.x = v;
                }
                if let Some(v) = get(p, 'Y') {
                    self.y = v;
                }
                if let Some(v) = get(p, 'Z') {
                    self.z = v;
                }
                if let Some(v) = get(p, 'E') {
                    self.e = v;
                }
            }
            "G0" | "G1" | "G2" | "G3" => {
                let (x0, y0, z0) = (self.x, self.y, self.z);
                let axis = |k: char, cur: f64, abs: bool| match get(p, k) {
                    None => cur,
                    Some(v) => {
                        if abs {
                            v
                        } else {
                            cur + v
                        }
                    }
                };
                let x1 = axis('X', x0, self.abs_xyz);
                let y1 = axis('Y', y0, self.abs_xyz);
                let z1 = axis('Z', z0, self.abs_xyz);
                let mut de = 0.0;
                if let Some(e) = get(p, 'E') {
                    if self.abs_e {
                        de = e - self.e;
                        self.e = e;
                    } else {
                        de = e;
                        self.e += de;
                    }
                }
                if let Some(f) = get(p, 'F') {
                    self.f = f;
                }
                self.x = x1;
                self.y = y1;
                self.z = z1;
                let mut mv = Move {
                    cmd: cmd.to_string(),
                    x0,
                    y0,
                    z0,
                    x1,
                    y1,
                    z1,
                    de,
                    f: self.f,
                    xy: (x1 - x0).abs() > 1e-9 || (y1 - y0).abs() > 1e-9,
                    i: 0.0,
                    j: 0.0,
                    turns: None,
                };
                if cmd == "G2" || cmd == "G3" {
                    mv.i = get(p, 'I').unwrap_or(0.0);
                    mv.j = get(p, 'J').unwrap_or(0.0);
                    mv.turns = get(p, 'P');
                    if !mv.xy && (mv.i != 0.0 || mv.j != 0.0) {
                        mv.xy = true;
                    }
                }
                return Some(mv);
            }
            _ => {}
        }
        None
    }
}

impl Default for Machine {
    fn default() -> Self {
        Self::new()
    }
}

/// `; FEATURE: x` / `; TYPE: x` -> lower-case name.
/// A part's outermost wall: "Outer wall" (Bambu Studio, OrcaSlicer), "External perimeter" (PrusaSlicer).
pub fn is_outer_wall(feature: &str) -> bool {
    feature.contains("outer wall") || feature.contains("external perimeter")
}

pub fn feature_of(s: &str) -> Option<String> {
    let t = s.strip_prefix(';')?.trim_start();
    let up = t.to_ascii_uppercase();
    let rest = if up.starts_with("FEATURE") {
        &t[7..]
    } else if up.starts_with("TYPE") {
        &t[4..]
    } else {
        return None;
    };
    let rest = rest.trim_start().strip_prefix(':')?;
    let name = rest.trim();
    if name.is_empty() {
        None
    } else {
        Some(name.to_lowercase())
    }
}

fn number_after(s: &str, keys: &[&str]) -> Option<f64> {
    let t = s.strip_prefix(';')?.trim_start();
    let up = t.to_ascii_uppercase();
    for k in keys {
        if up.starts_with(k) {
            let rest = t[k.len()..].trim_start().strip_prefix(':')?.trim_start();
            let end = rest
                .find(|c: char| !(c.is_ascii_digit() || c == '.' || c == '-'))
                .unwrap_or(rest.len());
            return rest[..end].parse().ok();
        }
    }
    None
}

pub fn is_layer_change(s: &str) -> bool {
    match s.strip_prefix(';') {
        Some(t) => {
            let up = t.trim_start().to_ascii_uppercase();
            for k in ["CHANGE_LAYER", "LAYER_CHANGE"] {
                if let Some(rest) = up.strip_prefix(k) {
                    // \b: next char not a word char
                    return !rest
                        .chars()
                        .next()
                        .map(|c| c.is_alphanumeric() || c == '_')
                        .unwrap_or(false);
                }
            }
            false
        }
        None => false,
    }
}

pub struct Parsed {
    pub lines: Vec<String>,
    pub blocks: Vec<Block>,
    pub features: Vec<(usize, String)>,
    /// layer -> other extrusions (x0, y0, x1, y1, width)
    pub others: HashMap<usize, Vec<Seg>>,
}

pub fn find_blocks(lines: Vec<String>, default_width: f64) -> Parsed {
    find_blocks_of(lines, default_width, &TOP_FEATURES)
}

/// The first layer's bottom surface (M20): [`BOTTOM_FEATURES`] blocks on the
/// first printed layer only. Bottom surfaces higher up (over supports or
/// bridges) face down into the print, not onto the plate, and are left out.
pub fn find_bottom(lines: Vec<String>, default_width: f64) -> Parsed {
    let mut p = find_blocks_of(lines, default_width, &BOTTOM_FEATURES);
    // layer 0 is the start G-code before the first layer change (a purge
    // line prints there): the first layer is the lowest after it, unless the
    // file marks no layers at all
    let printed = |l: usize| p.others.get(&l).is_some_and(|v| !v.is_empty());
    let layers: Vec<usize> = p
        .others
        .keys()
        .copied()
        .filter(|&l| printed(l))
        .chain(p.blocks.iter().map(|b| b.layer))
        .collect();
    let first = layers
        .iter()
        .copied()
        .filter(|&l| l > 0)
        .min()
        .or_else(|| layers.iter().copied().min());
    if let Some(f) = first {
        p.blocks.retain(|b| b.layer == f);
    }
    p
}

/// [`find_blocks`] for other features (the solid infill under a top surface).
pub fn find_blocks_of(lines: Vec<String>, default_width: f64, kinds: &[&str]) -> Parsed {
    let mut m = Machine::new();
    let mut blocks: Vec<Block> = Vec::new();
    let mut features = Vec::new();
    let mut others: HashMap<usize, Vec<Seg>> = HashMap::new();
    let mut in_top = false;
    let mut cur: Option<Block> = None;
    let mut width: Option<f64> = None;
    let mut height: Option<f64> = None;
    let mut layer = 0usize;
    let mut last_accel: Option<String> = None;

    fn close(cur: &mut Option<Block>, blocks: &mut Vec<Block>) {
        if let Some(b) = cur.take() {
            if b.first.is_some() {
                blocks.push(b);
            }
        }
    }

    let mut cur_outer = false;
    for (idx, line) in lines.iter().enumerate() {
        let s = line.trim();
        if s.starts_with(';') {
            if let Some(name) = feature_of(s) {
                close(&mut cur, &mut blocks);
                cur_outer = is_outer_wall(&name);
                features.push((idx, name.clone()));
                in_top = kinds.contains(&name.as_str());
                if in_top {
                    cur = Some(Block {
                        layer,
                        ..Default::default()
                    });
                }
                continue;
            }
            if is_layer_change(s) {
                close(&mut cur, &mut blocks);
                in_top = false;
                layer += 1;
                continue;
            }
            if let Some(v) = number_after(s, &["LINE_WIDTH", "WIDTH"]) {
                width = Some(v);
                continue;
            }
            if let Some(v) = number_after(s, &["LAYER_HEIGHT", "HEIGHT"]) {
                height = Some(v);
                continue;
            }
            if let Some(b) = cur.as_mut() {
                if b.first.is_some() && !s.to_ascii_uppercase().contains("WIPE") {
                    b.keep.push((idx, line.clone()));
                }
            }
            continue;
        }

        let parsed = parse_line(line);
        if let Some((cmd, _)) = &parsed {
            if cmd == "M204" && cur.as_ref().map(|b| b.first.is_none()).unwrap_or(true) {
                last_accel = Some(line.clone());
            }
        }
        let before = m.snapshot();
        let abs_e_before = m.abs_e;
        let mv = match &parsed {
            Some((cmd, p)) => m.apply(cmd, p),
            None => None,
        };
        let w_now = width.unwrap_or(default_width);

        if let Some(mv) = &mv {
            if !in_top && mv.xy && mv.de > 1e-7 {
                let pts = if (mv.cmd == "G2" || mv.cmd == "G3") && !mv.multi_turn() {
                    arc_points(mv.x0, mv.y0, mv.x1, mv.y1, mv.i, mv.j, mv.cmd == "G2", 0.5)
                } else {
                    vec![(mv.x1, mv.y1)]
                };
                let lst = others.entry(layer).or_default();
                let (mut px, mut py) = (mv.x0, mv.y0);
                for (qx, qy) in pts {
                    lst.push(Seg {
                        x0: px,
                        y0: py,
                        x1: qx,
                        y1: qy,
                        w: w_now,
                        e: 0.0,
                        outer: cur_outer,
                    });
                    px = qx;
                    py = qy;
                }
            }
        }

        let Some(b) = cur.as_mut() else { continue };
        let cmd = parsed.as_ref().map(|(c, _)| c.as_str()).unwrap_or("");
        if ABORT_CMDS.contains(&cmd)
            || (cmd.starts_with('T')
                && cmd.len() > 1
                && cmd[1..].chars().all(|c| c.is_ascii_digit()))
        {
            if b.first.is_some() && b.abort.is_none() {
                b.abort = Some(format!("contains {}", cmd));
            }
            continue;
        }
        let is_ext = mv.as_ref().map(|mv| mv.xy && mv.de > 1e-7).unwrap_or(false);
        if is_ext {
            let mv = mv.unwrap();
            if b.first.is_none() {
                b.first = Some(idx);
                b.start = before;
                b.abs_e = abs_e_before;
                b.accel = last_accel.clone();
                b.height = height;
            }
            if ((mv.z1 - b.start.2).abs() > 1e-4 || (mv.z0 - b.start.2).abs() > 1e-4)
                && b.abort.is_none()
            {
                b.abort = Some("extrusion at changing Z".into());
            }
            let pts = if mv.cmd == "G2" || mv.cmd == "G3" {
                if mv.multi_turn() {
                    if b.abort.is_none() {
                        b.abort = Some("multi-turn arc".into());
                    }
                    vec![(mv.x1, mv.y1)]
                } else {
                    arc_points(mv.x0, mv.y0, mv.x1, mv.y1, mv.i, mv.j, mv.cmd == "G2", 0.2)
                }
            } else {
                vec![(mv.x1, mv.y1)]
            };
            let (mut px, mut py) = (mv.x0, mv.y0);
            let mut pieces = Vec::with_capacity(pts.len());
            for (qx, qy) in pts {
                pieces.push((px, py, qx, qy, (qx - px).hypot(qy - py)));
                px = qx;
                py = qy;
            }
            let length: f64 = pieces.iter().map(|p| p.4).sum();
            for (ax, ay, bx, by, pl) in pieces {
                let share = if length > 0.0 {
                    mv.de * pl / length
                } else {
                    0.0
                };
                b.segments.push(Seg {
                    x0: ax,
                    y0: ay,
                    x1: bx,
                    y1: by,
                    w: w_now,
                    e: share,
                    outer: false,
                });
            }
            b.ext_len += length;
            b.ext_e += mv.de;
            match b.f_weight.iter_mut().find(|(f, _)| *f == mv.f) {
                Some(fw) => fw.1 += length,
                None => b.f_weight.push((mv.f, length)),
            }
            b.last = idx;
            b.end = m.snapshot();
            b.width = w_now;
            continue;
        }
        if b.first.is_some() && mv.is_none() && !cmd.is_empty() {
            b.keep.push((idx, line.clone()));
        }
    }
    close(&mut cur, &mut blocks);
    for b in blocks.iter_mut() {
        let (first, last) = (b.first.unwrap(), b.last);
        b.keep.retain(|(i, _)| first < *i && *i < last);
    }
    let _ = has; // (kept for parity with parse_line users)
    Parsed {
        lines,
        blocks,
        features,
        others,
    }
}

/// Lines of a G-code file, each with its line ending.
pub fn read_lines(text: &str) -> Vec<String> {
    text.split_inclusive('\n').map(|s| s.to_string()).collect()
}
