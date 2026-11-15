//! Raised tiles (ADR 0010, M22b): tiles of chosen tile groups stand a number
//! of layers above the top surface they lie on. Each raised layer of a tile
//! is a wall (one loop) round solid fill, at alternating 45° and 135° for
//! strength; its highest layer carries the pattern's lines.
//!
//! The raised moves go into the plate's own layer at that height when one is
//! printing (a taller piece on the plate), right after its move to that
//! height; above the plate's last layer they are new layers, added after the
//! print's last extrusion, and every absolute Z move after them (the end
//! G-code) is raised by the added height so the nozzle never comes back down
//! into the print. The printer ends each inserted run where it began (X, Y,
//! E, feed rate), as every rewritten block does.

use crate::engine::{order_paths, Args, Emitter, Path, Replacements};
use crate::fill::{self, PatternedAssembly};
use crate::gcode::{is_layer_change, parse_line, Machine};
use crate::geom::{self, MP};
use std::collections::BTreeMap;

/// A patterned top surface raised tiles stand on, as its rewrite found it.
#[derive(Debug, Clone)]
pub struct Top {
    pub z: f64,
    /// Its layer height and line width, spacing and shortest line (mm).
    pub h: f64,
    pub w: f64,
    pub s: f64,
    pub min_seg: f64,
    /// Plastic per mm of line, and the print feed rate (mm/min).
    pub e_per_mm: f64,
    pub print_f: f64,
    /// Where the pattern lies on it (inside its rims), bed mm.
    pub region: MP,
}

/// One raised layer's moves over one top.
#[derive(Debug, Clone)]
pub struct Level {
    pub z: f64,
    pub h: f64,
    pub paths: Vec<Path>,
    pub e_per_mm: f64,
    pub print_f: f64,
}

fn lines_path(segs: Vec<((f64, f64), (f64, f64))>, out: &mut Vec<Path>) {
    out.extend(segs.into_iter().map(|(p, q)| vec![(p, q, None)]));
}

fn loops(g: &MP) -> Vec<Path> {
    geom::rings(g)
        .into_iter()
        .map(|r| {
            r.0.windows(2)
                .map(|w| ((w[0].x, w[0].y), (w[1].x, w[1].y), None))
                .collect()
        })
        .collect()
}

/// The raised layers over a top: for each height above it, the wall and fill
/// of every tile raised at least that high. Tiles narrower than two line
/// widths can't take a wall and fill: they are counted in `thin` and left flat.
pub fn levels(
    pl: &mut PatternedAssembly,
    top: &Top,
    raise: &BTreeMap<String, usize>,
    thin: &mut usize,
) -> Vec<Level> {
    let most = raise.values().copied().max().unwrap_or(0);
    let mut by_level: Vec<Vec<Path>> = vec![Vec::new(); most];
    for (shape, group) in pl.tiles_within(&top.region) {
        let n = raise.get(&group).copied().unwrap_or(0);
        if n == 0 {
            continue;
        }
        let wall = geom::buffer(&shape, -top.w / 2.0, false);
        let inside = geom::buffer(&shape, -top.w, false);
        if geom::is_empty(&wall) || geom::area(&wall) < top.w * top.w {
            *thin += 1;
            continue;
        }
        for k in 1..=n {
            let paths = &mut by_level[k - 1];
            paths.extend(loops(&wall));
            if geom::is_empty(&inside) {
                continue;
            }
            if k == n {
                // the highest layer: the pattern's own lines for this tile
                let (found, _) = pl.paths(&inside, top.s, top.min_seg, true);
                paths.extend(found.into_iter().map(|(segs, _, _)| {
                    segs.into_iter()
                        .map(|(p, q)| (p, q, None))
                        .collect::<Path>()
                }));
            } else {
                let angle = if k % 2 == 1 { 45.0 } else { 135.0 };
                lines_path(fill::lines_at(&inside, angle, top.s, top.min_seg), paths);
            }
        }
    }
    by_level
        .into_iter()
        .enumerate()
        .filter(|(_, p)| !p.is_empty())
        .map(|(k, paths)| Level {
            z: top.z + (k + 1) as f64 * top.h,
            h: top.h,
            paths,
            e_per_mm: top.e_per_mm,
            print_f: top.print_f,
        })
        .collect()
}

/// Whether line `i` lies in a rewritten block.
fn inside(taken: &Replacements, i: usize) -> bool {
    taken
        .iter()
        .any(|(&first, (last, _))| first <= i && i <= *last)
}

fn key(z: f64) -> i64 {
    (z * 1000.0).round() as i64
}

/// What the printer is doing after a line: (x, y, z, e, f) and whether E is absolute.
type State = ((f64, f64, f64, f64, f64), bool);

fn emit(em: &mut Emitter, levels: &[&Level]) {
    for lv in levels {
        let paths = order_paths(lv.paths.clone(), (em.x, em.y));
        for path in paths {
            for &(p, q, e) in &path {
                em.travel(p.0, p.1);
                let de = e.unwrap_or_else(|| (q.0 - p.0).hypot(q.1 - p.1) * lv.e_per_mm);
                em.extrude(q.0, q.1, de, lv.print_f);
            }
        }
    }
}

/// Put the raised layers into the plate's lines: replacements to add to the
/// rewrite's, the layers added above the last one, the new top, and a log.
pub fn insert(
    lines: &[String],
    levels: Vec<Level>,
    args: &Args,
    taken: &Replacements,
) -> Result<(Replacements, usize, f64, String), String> {
    let mut log = String::new();
    let mut rep: Replacements = Replacements::new();
    if levels.is_empty() {
        return Ok((rep, 0, 0.0, log));
    }
    // every layer: its height and the line that moves to it, with the state after it
    let mut m = Machine::new();
    let mut layer_z: BTreeMap<i64, (usize, State)> = BTreeMap::new();
    let mut waiting = false;
    let mut last_extrusion: Option<(usize, State)> = None;
    let mut top_z = 0.0f64;
    let bambu = lines.iter().any(|l| l.starts_with("; CHANGE_LAYER"));
    for (i, line) in lines.iter().enumerate() {
        let s = line.trim();
        if s.starts_with(';') {
            if is_layer_change(s) {
                waiting = true;
            }
            continue;
        }
        let Some((cmd, p)) = parse_line(line) else {
            continue;
        };
        let before = (m.x, m.y, m.e);
        m.apply(&cmd, &p);
        let st: State = ((m.x, m.y, m.z, m.e, m.f), m.abs_e);
        if waiting && p.iter().any(|(c, _)| *c == 'Z') && (cmd == "G0" || cmd == "G1") {
            layer_z.entry(key(m.z)).or_insert((i, st));
            top_z = top_z.max(m.z);
            waiting = false;
        }
        let moved = (m.x - before.0).hypot(m.y - before.1) > 1e-6;
        let pushed = if m.abs_e {
            m.e > before.2 + 1e-9
        } else {
            p.iter()
                .any(|(c, v)| *c == 'E' && v.is_some_and(|e| e > 1e-9))
        };
        if (cmd == "G1" || cmd == "G2" || cmd == "G3") && moved && pushed {
            last_extrusion = Some((i, st));
        }
    }
    let Some((last_i, _)) = last_extrusion else {
        return Err("no extrusion found to raise tiles after".into());
    };
    // the end of the print's last move, including a rewritten block that holds it
    let mut after = last_i;
    for (&first, (last, _)) in taken {
        if first <= after && after <= *last {
            after = *last;
        }
    }
    let at_end = after + 1;
    if at_end >= lines.len() {
        return Err("no end G-code after the print to add raised layers before".into());
    }

    let mut group: BTreeMap<i64, Vec<&Level>> = BTreeMap::new();
    for lv in &levels {
        group.entry(key(lv.z)).or_default().push(lv);
    }
    let mut above: Vec<(f64, f64, Vec<&Level>)> = Vec::new();
    for (zk, lvs) in group {
        if zk > key(top_z) {
            above.push((lvs[0].z, lvs[0].h, lvs));
            continue;
        }
        match layer_z.get(&zk) {
            Some(&(i, (st, abs_e))) if !inside(taken, i) && !rep.contains_key(&i) => {
                let mut em = Emitter::new(st, abs_e, args);
                em.out.push(lines[i].clone());
                em.out.push("; tessafab: raised tiles\n".into());
                emit(&mut em, &lvs);
                em.finish(st);
                rep.insert(i, (i, em.out));
                log += &format!(
                    "relief: raised tiles at Z {:.3} in the plate's own layer\n",
                    lvs[0].z
                );
            }
            _ => {
                log += &format!(
                    "relief: no layer at Z {:.3} to put raised tiles in; that height is left out\n",
                    lvs[0].z
                );
            }
        }
    }

    let mut added = 0;
    let mut new_top = top_z;
    if !above.is_empty() {
        // the state before the end G-code: after the last printed line
        let mut m = Machine::new();
        for line in &lines[..at_end] {
            if let Some((cmd, p)) = parse_line(line) {
                m.apply(&cmd, &p);
            }
        }
        let st = (m.x, m.y, m.z, m.e, m.f);
        let mut em = Emitter::new(st, m.abs_e, args);
        em.out
            .push("; tessafab: raised tiles, added layers\n".into());
        for (z, h, lvs) in &above {
            if bambu {
                em.out.push("; CHANGE_LAYER\n".into());
                em.out
                    .push(format!("; Z_HEIGHT: {}\n", crate::engine::fmt(*z, 3)));
                em.out
                    .push(format!("; LAYER_HEIGHT: {}\n", crate::engine::fmt(*h, 3)));
            } else {
                em.out.push(";LAYER_CHANGE\n".into());
                em.out.push(format!(";Z:{}\n", crate::engine::fmt(*z, 3)));
                em.out
                    .push(format!(";HEIGHT:{}\n", crate::engine::fmt(*h, 3)));
            }
            em.out.push(format!(
                "G1 Z{} F{}\n",
                crate::engine::fmt(*z, 3),
                crate::engine::fmt(em.travel_f, 0)
            ));
            em.z = *z;
            emit(&mut em, lvs);
            added += 1;
            new_top = new_top.max(*z);
        }
        em.finish(st);
        em.out.push("; tessafab: end of raised tiles\n".into());
        // the end G-code, every absolute Z move raised by the added height
        let lift = new_top - top_z;
        let mut m = Machine::new();
        for line in &lines[..at_end] {
            if let Some((cmd, p)) = parse_line(line) {
                m.apply(&cmd, &p);
            }
        }
        let mut first = true;
        for (i, line) in lines.iter().enumerate().skip(at_end) {
            let shifted = match parse_line(line) {
                Some((cmd, p)) => {
                    let abs = m.abs_xyz;
                    m.apply(&cmd, &p);
                    // every move with an absolute Z: arcs too (Bambu Studio ends
                    // a layer with a spiral lift, `G3 Z… I… J… P1`)
                    if abs && matches!(cmd.as_str(), "G0" | "G1" | "G2" | "G3") && lift > 0.0 {
                        shift_z(line, lift, new_top + 2.0)
                    } else {
                        None
                    }
                }
                None => None,
            };
            let text = shifted.unwrap_or_else(|| line.clone());
            if first {
                let mut out = std::mem::take(&mut em.out);
                out.push(text);
                rep.insert(i, (i, out));
                first = false;
            } else if text != *line {
                rep.insert(i, (i, vec![text]));
            }
        }
        log += &format!(
            "relief: {added} layer(s) of raised tiles added above Z {top_z:.3}, up to Z {new_top:.3}; the end G-code lifted {lift:.3} mm\n"
        );
    }
    Ok((rep, added, new_top, log))
}

/// `line` with its Z raised by `by` when it is under `below` (None if it has
/// no Z, or one already well clear of the new top, such as a parking height,
/// which raising could push past the printer's reach).
fn shift_z(line: &str, by: f64, below: f64) -> Option<String> {
    let (code, comment) = match line.find(';') {
        Some(i) => (&line[..i], &line[i..]),
        None => (line.trim_end_matches('\n'), ""),
    };
    let mut found = false;
    let words: Vec<String> = code
        .split_whitespace()
        .map(
            |w| match w.strip_prefix('Z').and_then(|v| v.parse::<f64>().ok()) {
                Some(z) if z < below => {
                    found = true;
                    format!("Z{}", crate::engine::fmt(z + by, 3))
                }
                _ => w.to_string(),
            },
        )
        .collect();
    if !found {
        return None;
    }
    let mut out = words.join(" ");
    if !comment.is_empty() {
        out.push(' ');
        out.push_str(comment.trim_end_matches('\n'));
    }
    out.push('\n');
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn z_moves_near_the_top_are_lifted_and_parking_ones_kept() {
        assert_eq!(shift_z("G1 Z3.4 F600\n", 0.6, 5.6).unwrap(), "G1 Z4 F600\n");
        assert_eq!(
            shift_z("G3 Z3.4 I1.217 J0 P1  F30000\n", 0.6, 5.6).unwrap(),
            "G3 Z4 I1.217 J0 P1 F30000\n"
        );
        assert_eq!(shift_z("G1 Z250 F600 ; park\n", 0.6, 5.6), None);
        assert_eq!(shift_z("G1 X10 Y10\n", 0.6, 5.6), None);
    }
}
