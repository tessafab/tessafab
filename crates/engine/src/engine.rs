//! Rewriting a plate's top surfaces with a patterned assembly: a port of
//! trace_region / rims / rewrite_group / process_plate from the Python engine.
//! The lines inside the tiles come from `tessafab_pattern::fill`; this module
//! is the G-code target's part: the surface's region, rims, path order and
//! the G-code written for them.

use crate::colour::Plan;
use crate::direction::Direction;
use crate::fill::{PatternedAssembly, Tile};
use crate::gcode::{Block, Parsed, Seg, Snapshot};
use crate::geom::{self, MP};
use geo::{Area, BoundingRect, Coord, LineString, MultiPolygon, Point, Polygon, Rect};
use serde::Deserialize;
use std::collections::{BTreeMap, HashMap};

#[derive(Deserialize, Clone, Debug)]
pub struct Args {
    pub close: f64,
    pub extra_inset: f64,
    pub wall_overlap: f64,
    pub min_area: f64,
    pub min_coverage: f64,
    pub min_segment: Option<f64>,
    pub layer_height: Option<f64>,
    pub flow: f64,
    pub print_speed: Option<f64>,
    pub travel_speed: f64,
    pub retract: f64,
    pub retract_speed: f64,
    pub retract_threshold: f64,
    pub zhop: f64,
    pub max_z: Option<f64>,
    pub no_merge: Option<bool>,
    pub edge_rim: Option<usize>,
    pub hole_rim: Option<usize>,
    /// Outline loops round every surface before the pattern (`--border`).
    #[serde(default)]
    pub border: Option<usize>,
    /// Layers the grout is cut into, counting the top (M22, relief): 1 (the
    /// top only) when unset; deeper grout redraws the solid layers under the
    /// top with the top's tiles, grout and all. A patterned bottom takes the
    /// same depth: its solid layers above the first get its tiles too (no grout).
    #[serde(default)]
    pub grout_depth: Option<usize>,
    /// Raised tiles (M22b, relief): layers above the top, per tile group.
    #[serde(default)]
    pub raise: Option<BTreeMap<String, usize>>,
}

#[derive(Deserialize)]
struct JPoly {
    ext: Vec<[f64; 2]>,
    holes: Vec<Vec<[f64; 2]>>,
}

#[derive(Deserialize)]
struct JTile {
    ext: Vec<[f64; 2]>,
    holes: Vec<Vec<[f64; 2]>>,
    angle: Option<f64>,
    concentric: bool,
    color: String,
}

#[derive(Deserialize)]
struct JPattern {
    args: Args,
    pieces: Vec<JPoly>,
    transforms: Vec<Option<[f64; 6]>>,
    tiles: Vec<JTile>,
}

fn ring(c: &[[f64; 2]]) -> LineString<f64> {
    LineString::from(c.iter().map(|p| (p[0], p[1])).collect::<Vec<_>>())
}

pub fn load_pattern(json: &str) -> (Args, PatternedAssembly) {
    let j: JPattern = serde_json::from_str(json).expect("pattern json");
    let pieces: Vec<MP> = j
        .pieces
        .iter()
        .map(|p| {
            geom::poly_mp(&Polygon::new(
                ring(&p.ext),
                p.holes.iter().map(|h| ring(h)).collect(),
            ))
        })
        .collect();
    let tiles = j
        .tiles
        .iter()
        .map(|t| {
            let poly = Polygon::new(ring(&t.ext), t.holes.iter().map(|h| ring(h)).collect());
            let bbox = poly.bounding_rect().unwrap();
            Tile {
                poly,
                bbox,
                direction: Direction::from_parts(t.angle, t.concentric),
                color: t.color.clone(),
            }
        })
        .collect();
    (j.args, PatternedAssembly::new(pieces, j.transforms, tiles))
}

/// Rebuild the top surface's line-centre region (trace_region).
pub fn trace_region(
    segs: &[Seg],
    w: f64,
    close: f64,
    extra: f64,
    others: &[Seg],
    overlap: f64,
) -> (MP, MP) {
    let rects: Vec<Polygon<f64>> = segs
        .iter()
        .filter(|g| (g.x1 - g.x0).hypot(g.y1 - g.y0) > 1e-6)
        .map(|g| geom::seg_rect(g.x0, g.y0, g.x1, g.y1, g.w))
        .collect();
    let prof = std::env::var("PP_PROFILE").is_ok();
    let t = std::time::Instant::now();
    let lap = |name: &str| {
        if prof {
            eprintln!("    tr {:>8.3}s {}", t.elapsed().as_secs_f64(), name)
        }
    };
    let raw = geom::union_all(&rects);
    lap(&format!("raw union of {} rects", rects.len()));
    let plain = geom::buffer(&raw, -(w / 2.0 + extra), false);
    lap("plain");
    if close <= 0.0 || geom::is_empty(&plain) {
        return (raw, plain);
    }
    let bb = plain.bounding_rect().unwrap();
    let pad = close + w;
    let (minx, miny, maxx, maxy) = (
        bb.min().x - pad,
        bb.min().y - pad,
        bb.max().x + pad,
        bb.max().y + pad,
    );
    let near: Vec<&Seg> = others
        .iter()
        .filter(|o| {
            o.x0.max(o.x1) >= minx
                && o.x0.min(o.x1) <= maxx
                && o.y0.max(o.y1) >= miny
                && o.y0.min(o.y1) <= maxy
                && (o.x1 - o.x0).hypot(o.y1 - o.y0) > 1e-6
        })
        .collect();
    let ncaps = near.len();
    let keep_out = if near.is_empty() {
        None
    } else if std::env::var("PP_CAPSULES").is_ok() {
        let caps: Vec<Polygon<f64>> = near
            .iter()
            .map(|o| {
                geom::capsule(
                    o.x0,
                    o.y0,
                    o.x1,
                    o.y1,
                    o.w / 2.0 + w / 2.0 - overlap * w + extra,
                )
            })
            .collect();
        Some(geom::union_all(&caps))
    } else {
        Some(geom::stroke_segments(
            &near
                .iter()
                .map(|o| {
                    (
                        o.x0,
                        o.y0,
                        o.x1,
                        o.y1,
                        o.w / 2.0 + w / 2.0 - overlap * w + extra,
                    )
                })
                .collect::<Vec<_>>(),
        ))
    };
    lap(&format!("keep_out union of {} capsules", ncaps));
    if prof {
        let nv = |g: &MP| {
            g.iter()
                .map(|p| {
                    p.exterior().0.len() + p.interiors().iter().map(|r| r.0.len()).sum::<usize>()
                })
                .sum::<usize>()
        };
        if let Some(k) = &keep_out {
            eprintln!(
                "    tr keep_out: {} polygons, {} vertices; plain: {} polygons, {} vertices",
                k.0.len(),
                nv(k),
                plain.0.len(),
                nv(&plain)
            );
        }
    }
    let holes: Vec<(f64, f64)> = plain
        .iter()
        .flat_map(|p| {
            p.interiors()
                .iter()
                .map(|r| Polygon::new(r.clone(), vec![]))
        })
        .filter(|h| h.unsigned_area() > 1.0)
        .filter_map(|h| geom::representative_point(&h))
        .collect();
    let raw_area = geom::area(&raw);
    for c in [close, close * 0.93, close * 0.81] {
        let mut closed = geom::union(
            &geom::buffer(&geom::buffer(&plain, c, false), -c, false),
            &plain,
        );
        lap("closing");
        if prof {
            let nv = |g: &MP| {
                g.iter()
                    .map(|p| {
                        p.exterior().0.len()
                            + p.interiors().iter().map(|r| r.0.len()).sum::<usize>()
                    })
                    .sum::<usize>()
            };
            eprintln!(
                "    tr closed: {} polygons, {} vertices",
                closed.0.len(),
                nv(&closed)
            );
        }
        if let Some(k) = &keep_out {
            closed = geom::difference(&closed, k);
        }
        lap("minus keep_out");
        // Closing only extends the surface into notches. A piece of it that
        // touches none of the original surface is new: typically fill over the
        // open middle of a small hole, left when the hole's wall is cut out.
        // It would print in the air, so it goes (the Python engine instead
        // rejects the whole attempt, often losing the closing for the layer).
        closed = MultiPolygon::new(
            closed
                .into_iter()
                .filter(|q| {
                    let ov = geom::area(&geom::intersection(&geom::poly_mp(q), &plain));
                    if prof && ov <= 0.01 {
                        let b = q.bounding_rect().unwrap();
                        eprintln!("    tr dropped piece {:.3} mm2 (overlap {:.4}) at {:.1},{:.1}..{:.1},{:.1}", q.unsigned_area(), ov, b.min().x, b.min().y, b.max().x, b.max().y);
                    }
                    ov > 0.01
                })
                .collect(),
        );

        if geom::is_empty(&closed) || geom::area(&closed) > raw_area {
            if prof {
                eprintln!(
                    "    tr rejected: area {:.3} > raw {:.3}",
                    geom::area(&closed),
                    raw_area
                );
            }
            continue;
        }
        if let Some(h) = holes
            .iter()
            .find(|&&(x, y)| geom::contains_pt(&closed, x, y))
        {
            if prof {
                eprintln!(
                    "    tr rejected: covers hole at {:?} ({} holes)",
                    h,
                    holes.len()
                );
            }
            continue;
        }
        return (raw, geom::simplify(&closed, 0.02));
    }
    (raw, plain)
}

pub(crate) type Path = Vec<((f64, f64), (f64, f64), Option<f64>)>;

fn lines_to_paths(lines: Vec<LineString<f64>>, min_seg: f64, out: &mut Vec<Path>) {
    for g in lines {
        if geom::length(&g) < min_seg {
            continue;
        }
        out.push(
            g.0.windows(2)
                .map(|w| ((w[0].x, w[0].y), (w[1].x, w[1].y), None))
                .collect(),
        );
    }
}

/// Rim loops round the edges and holes of `fill` (postprocess.rims).
/// How much closer than a full spacing the pattern's first line may come to
/// the last rim line (as a share of the spacing). Pattern lines sit where the
/// tiles put them, so the first one lands anywhere from (1 - RIM_GAP) to
/// (2 - RIM_GAP) spacings past the last rim: at most 35% overlap, at worst a
/// gap of 1.65 spacings. The Python engine used 0.001 mm, which left grooves of
/// up to two spacings along edges for a line 3% too close; 0.25 still left one
/// along the long edges of narrow pieces.
pub const RIM_GAP: f64 = 0.35;

/// Concentric outline loops at the edge of `fill` (`--border`): the paths
/// and the region left inside them.
pub fn border_paths(fill: &MP, loops: usize, s: f64, min_seg: f64) -> (Vec<Path>, MP) {
    let mut paths = Vec::new();
    for k in 0..loops {
        let g = if k > 0 {
            geom::buffer(fill, -(k as f64) * s, false)
        } else {
            fill.clone()
        };
        for poly in g.iter() {
            for ring in std::iter::once(poly.exterior()).chain(poly.interiors()) {
                if geom::length(ring) < 2.0 * min_seg {
                    continue;
                }
                paths.push(
                    ring.0
                        .windows(2)
                        .map(|w| ((w[0].x, w[0].y), (w[1].x, w[1].y), None))
                        .collect(),
                );
            }
        }
    }
    let inner = if loops > 0 {
        geom::buffer(fill, -(loops as f64) * s, false)
    } else {
        fill.clone()
    };
    (paths, inner)
}

pub fn rims(fill: &MP, edge: usize, hole: usize, s: f64, min_seg: f64) -> (Vec<Path>, MP) {
    let min_hole = 1.0;
    if edge == 0 && hole == 0 {
        return (vec![], fill.clone());
    }
    let mut paths = Vec::new();
    let mut rest: Vec<Polygon<f64>> = Vec::new();
    for p in fill.iter() {
        let ints: Vec<Polygon<f64>> = p
            .interiors()
            .iter()
            .map(|r| Polygon::new(r.clone(), vec![]))
            .collect();
        let (big, small): (Vec<_>, Vec<_>) = if hole > 0 {
            ints.into_iter()
                .partition(|h| h.unsigned_area() >= min_hole)
        } else {
            (vec![], ints)
        };
        if edge == 0 && big.is_empty() {
            rest.push(p.clone());
            continue;
        }
        let pm = geom::poly_mp(p);
        let outer = geom::poly_mp(&geom::exterior_only(p));
        let e = if edge > 0 {
            geom::buffer(&geom::buffer(&outer, s, true), -s, true)
        } else {
            outer
        };
        let mut h = if big.is_empty() {
            geom::empty()
        } else {
            geom::buffer(&geom::buffer(&geom::union_all(&big), -s, true), s, true)
        };
        h = MultiPolygon::new(
            h.0.into_iter()
                .filter(|g| g.unsigned_area() >= min_hole)
                .collect(),
        );
        let small_mp = geom::union_all(&small);
        let clip = geom::buffer(
            &geom::union(&geom::difference(&geom::difference(&e, &h), &small_mp), &pm),
            1e-4,
            false,
        );
        let loops = edge.max(if geom::is_empty(&h) { 0 } else { hole });
        for k in 0..loops {
            let (ke, kh) = (k.min(edge), k.min(hole));
            let ek = if ke > 0 {
                geom::buffer(&e, -(ke as f64) * s, false)
            } else {
                e.clone()
            };
            let hk = if kh > 0 && !geom::is_empty(&h) {
                geom::buffer(&h, kh as f64 * s, false)
            } else {
                h.clone()
            };
            let r = geom::difference(&ek, &hk);
            let mut line = geom::rings(&r);
            if k >= edge {
                let band = geom::difference(
                    &geom::buffer(&ek, 1e-3, false),
                    &geom::buffer(&ek, -1e-3, false),
                );
                line = geom::clip_out(&band, &line);
            }
            if k >= hole && !geom::is_empty(&hk) {
                let band = geom::difference(
                    &geom::buffer(&hk, 1e-3, false),
                    &geom::buffer(&hk, -1e-3, false),
                );
                line = geom::clip_out(&band, &line);
            }
            let parts = geom::clip_lines(&clip, &line);
            lines_to_paths(
                if parts.is_empty() {
                    parts
                } else {
                    geom::linemerge(parts)
                },
                min_seg,
                &mut paths,
            );
        }
        let mut left = pm.clone();
        if edge > 0 {
            left = geom::intersection(
                &left,
                &geom::buffer(&e, -(edge as f64 * s - RIM_GAP * s), false),
            );
        }
        if !geom::is_empty(&h) {
            left = geom::difference(
                &left,
                &geom::buffer(&h, hole as f64 * s - RIM_GAP * s, false),
            );
        }
        if std::env::var("PP_PROFILE").is_ok() {
            let b = p.bounding_rect().unwrap();
            eprintln!("  rims {:.0},{:.0}..{:.0},{:.0}: p {:.1} (holes {} big {}), e {:.1}, h {:.2}, left {:.1}, exterior signed area {:.1}",
                b.min().x, b.min().y, b.max().x, b.max().y, p.unsigned_area(), p.interiors().len(), big.len(), geom::area(&e), geom::area(&h), geom::area(&left),
                geo::Area::signed_area(&geom::exterior_only(p)));
        }
        rest.extend(left.0.into_iter().filter(|g| g.unsigned_area() > 1e-3));
    }
    let rest = if rest.is_empty() {
        geom::empty()
    } else {
        geom::union_all(&rest)
    };
    (paths, rest)
}

pub(crate) fn order_paths(paths: Vec<Path>, start: (f64, f64)) -> Vec<Path> {
    let mut remaining: Vec<Option<Path>> = paths.into_iter().map(Some).collect();
    let mut left = remaining.len();
    let mut out = Vec::with_capacity(left);
    let (mut cx, mut cy) = start;
    while left > 0 {
        let mut best: Option<(f64, usize, bool)> = None;
        for (idx, p) in remaining.iter().enumerate() {
            let Some(segs) = p else { continue };
            let s0 = segs[0].0;
            let e0 = segs[segs.len() - 1].1;
            let ds = (s0.0 - cx).powi(2) + (s0.1 - cy).powi(2);
            let de = (e0.0 - cx).powi(2) + (e0.1 - cy).powi(2);
            if best.map(|b| ds < b.0).unwrap_or(true) {
                best = Some((ds, idx, false));
            }
            if de < best.unwrap().0 {
                best = Some((de, idx, true));
            }
        }
        let (_, idx, rev) = best.unwrap();
        let mut segs = remaining[idx].take().unwrap();
        left -= 1;
        if rev {
            segs = segs.into_iter().rev().map(|(p, q, e)| (q, p, e)).collect();
        }
        let end = segs[segs.len() - 1].1;
        cx = end.0;
        cy = end.1;
        out.push(segs);
    }
    out
}

fn poly_distance(p: &Polygon<f64>, pt: (f64, f64)) -> f64 {
    use geo::{Distance, Euclidean};
    Euclidean.distance(p, &Point::new(pt.0, pt.1))
}

fn order_by_island(
    raw: &MP,
    border: Vec<Path>,
    fill_paths: Vec<Path>,
    start: (f64, f64),
) -> Vec<Path> {
    let islands: Vec<&Polygon<f64>> = raw.iter().collect();
    if islands.len() <= 1 {
        let mut out = order_paths(border, start);
        let pos = out.last().map(|p| p[p.len() - 1].1).unwrap_or(start);
        out.extend(order_paths(fill_paths, pos));
        return out;
    }
    let boxes: Vec<Rect<f64>> = islands.iter().map(|p| p.bounding_rect().unwrap()).collect();
    let island_of = |p: (f64, f64), q: (f64, f64)| -> usize {
        let pt = ((p.0 + q.0) / 2.0, (p.1 + q.1) / 2.0);
        for (k, isl) in islands.iter().enumerate() {
            let b = boxes[k];
            if pt.0 >= b.min().x
                && pt.0 <= b.max().x
                && pt.1 >= b.min().y
                && pt.1 <= b.max().y
                && geo::Intersects::intersects(*isl, &Point::new(pt.0, pt.1))
            {
                return k;
            }
        }
        (0..islands.len())
            .min_by(|&a, &b| {
                poly_distance(islands[a], pt)
                    .partial_cmp(&poly_distance(islands[b], pt))
                    .unwrap()
            })
            .unwrap()
    };
    let mut groups: HashMap<usize, (Vec<Path>, Vec<Path>)> = HashMap::new();
    for (kind, paths) in [(0, border), (1, fill_paths)] {
        for path in paths {
            let mut parts: Vec<(usize, Path)> = Vec::new();
            for seg in path {
                let k = island_of(seg.0, seg.1);
                match parts.iter_mut().find(|(i, _)| *i == k) {
                    Some((_, v)) => v.push(seg),
                    None => parts.push((k, vec![seg])),
                }
            }
            for (k, segs) in parts {
                let g = groups.entry(k).or_default();
                if kind == 0 {
                    g.0.push(segs)
                } else {
                    g.1.push(segs)
                }
            }
        }
    }
    let mut out = Vec::new();
    let mut pos = start;
    let mut todo: Vec<usize> = groups.keys().cloned().collect();
    todo.sort();
    while !todo.is_empty() {
        let (ti, _) = todo
            .iter()
            .enumerate()
            .min_by(|a, b| {
                poly_distance(islands[*a.1], pos)
                    .partial_cmp(&poly_distance(islands[*b.1], pos))
                    .unwrap()
            })
            .unwrap();
        let k = todo.remove(ti);
        let (b, f) = groups.remove(&k).unwrap();
        let part = order_paths(b, pos);
        if let Some(p) = part.last() {
            pos = p[p.len() - 1].1;
        }
        let rest = order_paths(f, pos);
        if let Some(p) = rest.last() {
            pos = p[p.len() - 1].1;
        }
        out.extend(part);
        out.extend(rest);
    }
    out
}

pub fn fmt(v: f64, nd: usize) -> String {
    let mut s = format!("{:.*}", nd, v);
    if s.contains('.') {
        while s.ends_with('0') {
            s.pop();
        }
        if s.ends_with('.') {
            s.pop();
        }
    }
    if s == "-0" || s.is_empty() {
        s = "0".into();
    }
    s
}

pub(crate) struct Emitter<'a> {
    pub(crate) x: f64,
    pub(crate) y: f64,
    pub(crate) z: f64,
    e: f64,
    abs_e: bool,
    f: Option<f64>,
    pub(crate) out: Vec<String>,
    args: &'a Args,
    pub(crate) travel_f: f64,
    retract_f: f64,
}

impl<'a> Emitter<'a> {
    pub(crate) fn new(start: Snapshot, abs_e: bool, args: &'a Args) -> Self {
        Emitter {
            x: start.0,
            y: start.1,
            z: start.2,
            e: start.3,
            abs_e,
            f: None,
            out: Vec::new(),
            args,
            travel_f: args.travel_speed * 60.0,
            retract_f: args.retract_speed * 60.0,
        }
    }

    fn e_str(&mut self, delta: f64) -> String {
        if self.abs_e {
            self.e += delta;
            fmt(self.e, 5)
        } else {
            fmt(delta, 5)
        }
    }

    pub(crate) fn travel(&mut self, px: f64, py: f64) {
        let a = self.args;
        let d = (px - self.x).hypot(py - self.y);
        if d < 1e-4 {
            return;
        }
        let tf = fmt(self.travel_f, 0);
        if d > a.retract_threshold {
            if a.retract > 0.0 {
                let e = self.e_str(-a.retract);
                self.out
                    .push(format!("G1 E{} F{}\n", e, fmt(self.retract_f, 0)));
            }
            if a.zhop > 0.0 {
                self.out
                    .push(format!("G1 Z{} F{}\n", fmt(self.z + a.zhop, 3), tf));
            }
            self.out
                .push(format!("G1 X{} Y{} F{}\n", fmt(px, 3), fmt(py, 3), tf));
            if a.zhop > 0.0 {
                self.out.push(format!("G1 Z{}\n", fmt(self.z, 3)));
            }
            if a.retract > 0.0 {
                let e = self.e_str(a.retract);
                self.out
                    .push(format!("G1 E{} F{}\n", e, fmt(self.retract_f, 0)));
            }
        } else {
            self.out
                .push(format!("G1 X{} Y{} F{}\n", fmt(px, 3), fmt(py, 3), tf));
        }
        self.f = None;
        self.x = px;
        self.y = py;
    }

    pub(crate) fn extrude(&mut self, px: f64, py: f64, de: f64, f: f64) {
        let e = self.e_str(de);
        let mut line = format!("G1 X{} Y{} E{}", fmt(px, 3), fmt(py, 3), e);
        if self.f != Some(f) {
            line += &format!(" F{}", fmt(f, 0));
            self.f = Some(f);
        }
        line.push('\n');
        self.out.push(line);
        self.x = px;
        self.y = py;
    }

    pub(crate) fn finish(&mut self, end: Snapshot) {
        self.travel(end.0, end.1);
        if self.abs_e {
            self.out.push(format!("G92 E{}\n", fmt(end.3, 5)));
        }
        // the slicer's feed rate as it wrote it: some write fractions (F9466.211),
        // and the export checks the printer is left exactly where it was
        self.out.push(format!("G1 F{}\n", fmt(end.4, 4)));
    }
}

pub fn raw_of(b: &Block) -> MP {
    let rects: Vec<Polygon<f64>> = b
        .segments
        .iter()
        .filter(|g| (g.x1 - g.x0).hypot(g.y1 - g.y0) > 1e-6)
        .map(|g| geom::seg_rect(g.x0, g.y0, g.x1, g.y1, g.w))
        .collect();
    geom::union_all(&rects)
}

pub(crate) type Replacements = HashMap<usize, (usize, Vec<String>)>;

fn keep_segs(segs: &[Seg], area: &MP) -> Path {
    segs.iter()
        .filter(|g| geom::contains_pt(area, (g.x0 + g.x1) / 2.0, (g.y0 + g.y1) / 2.0))
        .map(|g| ((g.x0, g.y0), (g.x1, g.y1), Some(g.e)))
        .collect()
}

/// rewrite_group, placement path.
fn rewrite_group(
    blocks: &[&Block],
    args: &Args,
    others: &[Seg],
    cover: Option<&MP>,
    pl: &mut PatternedAssembly,
    plan: Option<&mut Plan>,
    tops: Option<&mut Vec<crate::relief::Top>>,
) -> Result<(Replacements, String), String> {
    let segs: Vec<Seg> = blocks
        .iter()
        .flat_map(|b| b.segments.iter().cloned())
        .collect();
    let ext_len: f64 = blocks.iter().map(|b| b.ext_len).sum();
    let ext_e: f64 = blocks.iter().map(|b| b.ext_e).sum();
    if segs.is_empty() || ext_len < 1e-3 {
        return Err("no extrusions".into());
    }
    let main = blocks
        .iter()
        .max_by(|a, b| a.ext_len.partial_cmp(&b.ext_len).unwrap())
        .unwrap();
    let w = main.width;
    let h = main.height.or(args.layer_height).unwrap_or(0.0);
    let spacing0 = if h > 0.0 && h < w {
        w - h * (1.0 - std::f64::consts::PI / 4.0)
    } else {
        w * 0.95
    };
    let s = spacing0;
    let min_seg = args.min_segment.unwrap_or(w);

    let prof = std::env::var("PP_PROFILE").is_ok();
    let t = std::time::Instant::now();
    let lap = |name: &str| {
        if prof {
            eprintln!("  {:>8.3}s {}", t.elapsed().as_secs_f64(), name)
        }
    };
    let (raw, mut fill) = trace_region(
        &segs,
        w,
        args.close * w,
        args.extra_inset,
        others,
        args.wall_overlap,
    );
    lap("trace_region");

    let mut keep_paths: Vec<Path> = Vec::new();
    let mut small: Vec<Polygon<f64>> = raw
        .iter()
        .filter(|p| p.unsigned_area() < args.min_area)
        .cloned()
        .collect();
    let mut hidden = 0;
    if let Some(cover) = cover {
        if !geom::is_empty(cover) {
            let cb = cover.bounding_rect().unwrap();
            for p in raw.iter() {
                if p.unsigned_area() < args.min_area {
                    continue;
                }
                let pb = p.bounding_rect().unwrap();
                if !geom::bbox_hit(&cb, &pb) {
                    continue;
                }
                let a = geom::area(&geom::intersection(&geom::poly_mp(p), cover));
                if a > 0.5 * p.unsigned_area() {
                    small.push(p.clone());
                    hidden += 1;
                }
            }
        }
    }
    if !small.is_empty() {
        let sm = geom::union_all(&small);
        let orig = keep_segs(&segs, &sm);
        if !orig.is_empty() {
            keep_paths.push(orig);
        }
        fill = geom::difference(&fill, &geom::buffer(&sm, w, true));
        fill = geom::polys_area_over(fill, 1.0);
    }
    if geom::is_empty(&fill) && keep_paths.is_empty() {
        return Err(format!("area {:.1} mm2 below --min-area", geom::area(&raw)));
    }

    lap("small/hidden");
    let (mut border, mut inner) = if geom::is_empty(&fill) {
        (Vec::new(), fill.clone())
    } else {
        border_paths(&fill, args.border.unwrap_or(0), s, min_seg)
    };
    let edge_rim = args.edge_rim.unwrap_or(2);
    let hole_rim = args.hole_rim.unwrap_or(2);
    if (edge_rim > 0 || hole_rim > 0) && !geom::is_empty(&inner) {
        let isl: Vec<Polygon<f64>> = inner
            .iter()
            .filter(|i| pl.piece_of(i).is_some())
            .cloned()
            .collect();
        if !isl.is_empty() {
            let target = MultiPolygon::new(isl);
            let (rp, rest) = rims(&target, edge_rim, hole_rim, s, min_seg);
            border.extend(rp);
            inner = geom::union(&geom::difference(&inner, &target), &rest);
        }
    }

    lap("rims");
    if prof {
        eprintln!(
            "  distinct tile shapes so far: {}, lookups {}",
            pl.shape_cache.values().map(|v| v.len()).sum::<usize>(),
            pl.shape_lookups
        );
    }
    if prof {
        for isl in fill.iter() {
            let b = isl.bounding_rect().unwrap();
            eprintln!(
                "  fill island {:.1} mm2 at {:.0},{:.0}..{:.0},{:.0} piece {:?}",
                isl.unsigned_area(),
                b.min().x,
                b.min().y,
                b.max().x,
                b.max().y,
                pl.piece_of(isl)
            );
        }
    }
    let pattern_area = inner.clone();
    let base_e = ext_e / ext_len * args.flow;
    let mut planks: Vec<Path> = Vec::new();
    let mut plank_colours: Vec<String> = Vec::new();
    let mut unplaced_n = 0;
    if !geom::is_empty(&inner) {
        let (found, unplaced) = pl.paths(&inner, s, min_seg, edge_rim > 0 || hole_rim > 0);
        for (path, sc, colour) in found {
            planks.push(
                path.into_iter()
                    .map(|(p, q)| (p, q, Some((q.0 - p.0).hypot(q.1 - p.1) * base_e * sc)))
                    .collect(),
            );
            plank_colours.push(colour);
        }
        if !unplaced.is_empty() {
            unplaced_n = unplaced.len();
            if prof {
                for u in &unplaced {
                    eprintln!(
                        "  unplaced island area {:.5} mm2 bbox {:?}",
                        u.unsigned_area(),
                        u.bounding_rect()
                            .map(|b| (b.min().x, b.min().y, b.max().x, b.max().y))
                    );
                }
            }
            let up = geom::buffer(&geom::union_all(&unplaced), w, true);
            let orig = keep_segs(&segs, &up);
            if !orig.is_empty() {
                keep_paths.push(orig);
            }
        }
    }
    if planks.is_empty() && border.is_empty() {
        if hidden > 0 || !keep_paths.is_empty() {
            return Err("only hidden or small surfaces, nothing to refill".into());
        }
        return Err("no pattern lines fit".into());
    }

    lap("pattern paths");
    let last = blocks[blocks.len() - 1];
    let start_xy = (last.start.0, last.start.1);
    let n_planks = planks.len();
    let n_border = border.len();
    // Multi-colour: the base filament's tiles print first, every other
    // filament's tiles after a tool change of their own.
    let mut by_filament: BTreeMap<usize, Vec<Path>> = BTreeMap::new();
    let mut fills = Vec::new();
    match plan.as_deref() {
        Some(plan) if plan.uses_colour() => {
            for (path, colour) in planks.into_iter().zip(&plank_colours) {
                let f = plan.filament_of(colour);
                if f == plan.base {
                    fills.push(path);
                } else {
                    by_filament.entry(f).or_default().push(path);
                }
            }
        }
        _ => fills = planks,
    }
    fills.extend(keep_paths);
    let ordered = order_by_island(&raw, border, fills, start_xy);

    lap("order");
    let new_len: f64 = ordered
        .iter()
        .chain(by_filament.values().flatten())
        .flatten()
        .map(|(p, q, _)| (q.0 - p.0).hypot(q.1 - p.1))
        .sum();
    if new_len * s < args.min_coverage * ext_len * spacing0 {
        return Err(format!(
            "coverage check failed ({:.0}% of original)",
            100.0 * new_len * s / (ext_len * spacing0)
        ));
    }
    let e_per_mm = ext_e / ext_len * (s / spacing0).clamp(0.85, 1.15) * args.flow;
    let mut fw: Vec<(f64, f64)> = Vec::new();
    for b in blocks {
        for &(f, l) in &b.f_weight {
            match fw.iter_mut().find(|x| x.0 == f) {
                Some(x) => x.1 += l,
                None => fw.push((f, l)),
            }
        }
    }
    let print_f = match args.print_speed {
        Some(v) if v > 0.0 => v * 60.0,
        _ => {
            fw.iter()
                .fold(
                    (0.0, -1.0),
                    |best, &(f, l)| if l > best.1 { (f, l) } else { best },
                )
                .0
        }
    };

    let mut rep: Replacements = HashMap::new();
    for b in &blocks[..blocks.len() - 1] {
        let mut em = Emitter::new(b.start, b.abs_e, args);
        em.out
            .push("; tessafab: top surface moved to end of layer\n".into());
        em.out.extend(b.keep.iter().map(|(_, l)| l.clone()));
        em.finish(b.end);
        rep.insert(b.first.unwrap(), (b.last, em.out));
    }
    let mut em = Emitter::new(last.start, last.abs_e, args);
    em.out.push(format!(
        "; tessafab: begin (SVG pattern spike, {} border, {} tile paths)\n",
        n_border, n_planks
    ));
    for b in blocks {
        em.out.extend(b.keep.iter().map(|(_, l)| l.clone()));
    }
    let any_m204 = blocks.iter().any(|b| {
        b.keep
            .iter()
            .any(|(_, l)| l.trim_start().to_ascii_uppercase().starts_with("M204"))
    });
    if let (Some(acc), true) = (&last.accel, any_m204) {
        em.out.push(acc.clone());
    }
    let emit = |em: &mut Emitter, paths: &[Path]| {
        for path in paths {
            for &(p, q, e) in path {
                em.travel(p.0, p.1);
                let de = e.unwrap_or_else(|| (q.0 - p.0).hypot(q.1 - p.1) * e_per_mm);
                em.extrude(q.0, q.1, de, print_f);
            }
        }
    };
    emit(&mut em, &ordered);
    if let (false, Some(plan)) = (by_filament.is_empty(), plan) {
        let z = last.start.2;
        let block = last.first.unwrap_or(0);
        let mut current = plan.base;
        for (f, paths) in by_filament {
            let paths = order_paths(paths, (em.x, em.y));
            let to = paths[0][0].0;
            em.out.extend(
                plan.change(current, f, to, z, last.layer, block)
                    .map_err(|e| e.0)?,
            );
            (em.x, em.y, em.f) = (to.0, to.1, None);
            current = f;
            emit(&mut em, &paths);
        }
        let to = (last.end.0, last.end.1);
        em.out.extend(
            plan.change(current, plan.base, to, z, last.layer, block)
                .map_err(|e| e.0)?,
        );
        (em.x, em.y, em.f) = (to.0, to.1, None);
    }
    em.finish(last.end);
    if let Some((_, l)) = last
        .keep
        .iter()
        .rev()
        .find(|(_, l)| l.trim_start().to_ascii_uppercase().starts_with("M204"))
    {
        em.out.push(l.clone());
    }
    em.out.push("; tessafab: end\n".into());
    rep.insert(last.first.unwrap(), (last.last, em.out));
    let mut info = format!(
        "{} block(s), {} planks, {} border loops, {:.0} mm of lines; SVG pattern",
        blocks.len(),
        n_planks,
        n_border,
        new_len
    );
    if unplaced_n > 0 {
        info += &format!(
            ", {} surface(s) on unmatched parts kept as sliced",
            unplaced_n
        );
    }
    if hidden > 0 {
        info += &format!(", {} surface(s) hidden from above kept as sliced", hidden);
    }
    if let Some(tops) = tops {
        tops.push(crate::relief::Top {
            z: last.start.2,
            h,
            w,
            s,
            min_seg,
            e_per_mm,
            print_f,
            region: pattern_area,
        });
    }
    Ok((rep, info))
}

/// A solid layer over a coloured bottom (bottom colour depth, M20b): the
/// slicer's own lines, kept as they run, each part printed in the filament of
/// the tile it lies under; the base filament's parts first, then each other
/// filament's after a tool change of its own, then back to the base.
fn recolour_group(
    blocks: &[&Block],
    args: &Args,
    region: &MP,
    pl: &PatternedAssembly,
    plan: &mut Plan,
) -> Result<(Replacements, String), String> {
    let segs: Vec<Seg> = blocks
        .iter()
        .flat_map(|b| b.segments.iter().cloned())
        .collect();
    let ext_len: f64 = blocks.iter().map(|b| b.ext_len).sum();
    let ext_e: f64 = blocks.iter().map(|b| b.ext_e).sum();
    if segs.is_empty() || ext_len < 1e-3 {
        return Err("no extrusions".into());
    }
    let e_per_mm = ext_e / ext_len * args.flow;
    let mut fw: Vec<(f64, f64)> = Vec::new();
    for b in blocks {
        for &(f, l) in &b.f_weight {
            match fw.iter_mut().find(|x| x.0 == f) {
                Some(x) => x.1 += l,
                None => fw.push((f, l)),
            }
        }
    }
    let print_f = fw
        .iter()
        .fold(
            (0.0, -1.0),
            |best, &(f, l)| if l > best.1 { (f, l) } else { best },
        )
        .0;
    // the tiles' areas, by filament (other than the base)
    let mut by_f: BTreeMap<usize, Vec<Polygon<f64>>> = BTreeMap::new();
    for (shape, group) in pl.tiles_within(region) {
        let f = plan.filament_of(&group);
        if f != plan.base {
            by_f.entry(f).or_default().extend(shape.0);
        }
    }
    if by_f.is_empty() {
        return Err("no coloured tiles over it".into());
    }
    let areas: BTreeMap<usize, MP> = by_f
        .into_iter()
        .map(|(f, v)| (f, geom::union_all(&v)))
        .collect();
    let all = geom::union_all(
        &areas
            .values()
            .flat_map(|a| a.0.iter().cloned())
            .collect::<Vec<_>>(),
    );
    let lines: Vec<LineString<f64>> = segs
        .iter()
        .filter(|g| (g.x1 - g.x0).hypot(g.y1 - g.y0) > 1e-6)
        .map(|g| LineString::from(vec![(g.x0, g.y0), (g.x1, g.y1)]))
        .collect();
    let to_paths = |ls: Vec<LineString<f64>>| -> Vec<Path> {
        ls.into_iter()
            .filter(|l| geom::length(l) > 0.05)
            .map(|l| {
                l.0.windows(2)
                    .map(|w| ((w[0].x, w[0].y), (w[1].x, w[1].y), None))
                    .collect()
            })
            .collect()
    };
    let last = blocks[blocks.len() - 1];
    let base = order_paths(
        to_paths(geom::clip_out(&all, &lines)),
        (last.start.0, last.start.1),
    );
    let mut rep: Replacements = HashMap::new();
    for b in &blocks[..blocks.len() - 1] {
        let mut em = Emitter::new(b.start, b.abs_e, args);
        em.out
            .push("; tessafab: solid layer moved to end of layer\n".into());
        em.out.extend(b.keep.iter().map(|(_, l)| l.clone()));
        em.finish(b.end);
        rep.insert(b.first.unwrap(), (b.last, em.out));
    }
    let mut em = Emitter::new(last.start, last.abs_e, args);
    em.out
        .push("; tessafab: begin (solid layer over a coloured bottom)\n".into());
    for b in blocks {
        em.out.extend(b.keep.iter().map(|(_, l)| l.clone()));
    }
    let emit = |em: &mut Emitter, paths: &[Path]| {
        for path in paths {
            for &(p, q, _) in path {
                em.travel(p.0, p.1);
                em.extrude(q.0, q.1, (q.0 - p.0).hypot(q.1 - p.1) * e_per_mm, print_f);
            }
        }
    };
    emit(&mut em, &base);
    let (z, block) = (last.start.2, last.first.unwrap_or(0));
    let mut current = plan.base;
    let mut parts = 0;
    for (f, area) in &areas {
        let paths = order_paths(to_paths(geom::clip_lines(area, &lines)), (em.x, em.y));
        if paths.is_empty() {
            continue;
        }
        let to = paths[0][0].0;
        em.out.extend(
            plan.change(current, *f, to, z, last.layer, block)
                .map_err(|e| e.0)?,
        );
        (em.x, em.y, em.f) = (to.0, to.1, None);
        current = *f;
        emit(&mut em, &paths);
        parts += paths.len();
    }
    if current != plan.base {
        let to = (last.end.0, last.end.1);
        em.out.extend(
            plan.change(current, plan.base, to, z, last.layer, block)
                .map_err(|e| e.0)?,
        );
        (em.x, em.y, em.f) = (to.0, to.1, None);
    }
    em.finish(last.end);
    em.out.push("; tessafab: end\n".into());
    rep.insert(last.first.unwrap(), (last.last, em.out));
    Ok((
        rep,
        format!(
            "{} block(s), {} line(s) in the base filament, {} in the tiles' filaments",
            blocks.len(),
            base.len(),
            parts
        ),
    ))
}

/// Which top-surface groups to rewrite: false keeps a group's original lines.
pub type Select<'a> = &'a dyn Fn(&[&Block]) -> bool;

/// process_plate: rewrite every usable top surface group; returns (lines, done, kept, log).
pub fn process_plate(
    p: &Parsed,
    args: &Args,
    pl: &mut PatternedAssembly,
) -> (Vec<String>, usize, usize, String) {
    process_plate_with(p, args, pl, None, &mut |_, _| true, None)
}

/// The first layer's bottom surface to pattern as well (M20): its blocks,
/// found in the same lines as the top surfaces; the assembly seen from below
/// (no grout: a gap in the first layer would print as a hole); and which of
/// its surfaces are chosen. It prints in the filament loaded at the time.
pub struct Bottom<'a> {
    pub parsed: &'a Parsed,
    pub pl: &'a mut PatternedAssembly,
    pub select: Option<Select<'a>>,
    /// Bottom colour depth (M20b): layers from the plate drawn in the tile
    /// groups' filaments when the plate prints colour tiles; 1, the first
    /// layer alone, by default. Above the relief depth (`Args::grout_depth`,
    /// whose layers get the tiles' own lines) the layers keep the slicer's
    /// lines (their directions alternate, for strength): only their colours
    /// follow the tiles.
    pub depth: usize,
}

/// Multi-colour tiles for a plate: the plan, and the placed pattern for the
/// layers under a coloured top (the same tiles without grout).
pub struct Colour<'p, 'a> {
    pub plan: &'p mut Plan<'a>,
    pub depth: Option<&'p mut PatternedAssembly>,
}

/// Solid-infill blocks to redraw as tiles under coloured top surfaces
/// (colour depth > 1): for every top surface that will be patterned, the
/// solid infill of the depth - 1 layers below it, where it lies under it.
fn colour_depth_groups<'s>(
    solid: &'s Parsed,
    args: &Args,
    top_groups: &[Vec<&Block>],
    select: Option<Select>,
    depth: usize,
) -> Vec<Vec<&'s Block>> {
    let mut under: BTreeMap<usize, Vec<Polygon<f64>>> = BTreeMap::new();
    for g in top_groups {
        let z = g[0].start.2;
        if args.max_z.is_some_and(|mz| z > mz + 1e-6) || select.is_some_and(|f| !f(g)) {
            continue;
        }
        let area: Vec<Polygon<f64>> = g.iter().flat_map(|b| raw_of(b).0).collect();
        if area.is_empty() {
            continue;
        }
        let region = geom::union_all(&area);
        for k in 1..depth {
            if let Some(l) = g[0].layer.checked_sub(k) {
                under.entry(l).or_default().extend(region.0.iter().cloned());
            }
        }
    }
    let mut keys: Vec<(usize, i64)> = Vec::new();
    let mut out: Vec<Vec<&Block>> = Vec::new();
    let unions: BTreeMap<usize, MP> = under
        .iter()
        .map(|(l, v)| (*l, geom::union_all(v)))
        .collect();
    for b in &solid.blocks {
        if b.abort.is_some() {
            continue;
        }
        let Some(u) = unions.get(&b.layer) else {
            continue;
        };
        let area = raw_of(b);
        if geom::is_empty(&area) || geom::area(&geom::intersection(&area, u)) < 1.0 {
            continue;
        }
        let key = (b.layer, (b.start.2 * 1000.0).round() as i64);
        match keys.iter().position(|k| *k == key) {
            Some(i) => out[i].push(b),
            None => {
                keys.push(key);
                out.push(vec![b]);
            }
        }
    }
    out
}

/// [`process_plate`] with a selection of surfaces and progress
/// (`progress(groups done, groups)`, called before each group; returning
/// false stops: the rest of the plate keeps the slicer's lines).
pub fn process_plate_with(
    p: &Parsed,
    args: &Args,
    pl: &mut PatternedAssembly,
    select: Option<Select>,
    progress: &mut dyn FnMut(usize, usize) -> bool,
    colour: Option<Colour>,
) -> (Vec<String>, usize, usize, String) {
    let (out, done, kept, log, _) =
        process_plate_faces(p, args, pl, select, progress, colour, None);
    (out, done, kept, log)
}

/// [`process_plate_with`], and the first layer's bottom surface too.
pub fn process_plate_faces(
    p: &Parsed,
    args: &Args,
    pl: &mut PatternedAssembly,
    select: Option<Select>,
    progress: &mut dyn FnMut(usize, usize) -> bool,
    mut colour: Option<Colour>,
    bottom: Option<Bottom>,
) -> (Vec<String>, usize, usize, String, usize) {
    let mut log = String::new();
    let ironing: Vec<usize> = p
        .features
        .iter()
        .filter(|(_, n)| n.contains("ironing"))
        .map(|(i, _)| *i)
        .collect();
    let mut groups: Vec<Vec<&Block>> = Vec::new();
    let mut key_of: Vec<(usize, i64)> = Vec::new();
    let mut skipped = 0;
    for b in &p.blocks {
        if let Some(why) = &b.abort {
            log += &format!("layer {}: kept original top surface ({})\n", b.layer, why);
            skipped += 1;
            continue;
        }
        let key = (b.layer, (b.start.2 * 1000.0).round() as i64);
        match key_of.iter().position(|k| *k == key) {
            Some(i) => groups[i].push(b),
            None => {
                key_of.push(key);
                groups.push(vec![b]);
            }
        }
    }
    let mut final_groups: Vec<Vec<&Block>> = Vec::new();
    for g in groups {
        let (lo, hi) = (g[0].first.unwrap(), g[g.len() - 1].last);
        if args.no_merge.unwrap_or(false) || ironing.iter().any(|&i| lo <= i && i <= hi) {
            final_groups.extend(g.into_iter().map(|b| vec![b]));
        } else {
            final_groups.push(g);
        }
    }

    // the solid infill under the tops, in print order: for colour depth (in
    // the tile groups' filaments) and for deeper grout (the grout cut on down)
    let colour_depth = colour
        .as_ref()
        .filter(|c| c.plan.uses_colour())
        .map_or(1, |c| c.plan.depth);
    let grout_depth = args.grout_depth.unwrap_or(1).max(1);
    let depth = colour_depth.max(grout_depth);
    let solid = (depth > 1).then(|| {
        crate::gcode::find_blocks_of(p.lines.clone(), 0.42, &crate::gcode::SOLID_FEATURES)
    });
    let key = |g: &[&Block]| (g[0].layer, (g[0].start.2 * 1000.0).round() as i64);
    let mut colour_keys: Vec<(usize, i64)> = Vec::new();
    let mut grout_keys: Vec<(usize, i64)> = Vec::new();
    let mut all: Vec<(Vec<&Block>, bool)> =
        final_groups.iter().map(|g| (g.clone(), false)).collect();
    if let Some(solid) = &solid {
        let under = |d: usize| -> Vec<(usize, i64)> {
            if d > 1 {
                colour_depth_groups(solid, args, &final_groups, select, d)
                    .iter()
                    .map(|g| key(g))
                    .collect()
            } else {
                Vec::new()
            }
        };
        colour_keys = under(colour_depth);
        grout_keys = under(grout_depth);
        all.extend(
            colour_depth_groups(solid, args, &final_groups, select, depth)
                .into_iter()
                .map(|g| (g, true)),
        );
        all.sort_by_key(|(g, _)| g[0].first.unwrap_or(0));
    }

    let mut raws: Option<Vec<(usize, MP)>> = None;
    let mut replacements: Replacements = HashMap::new();
    // raised tiles (M22b): the patterned tops they stand on, as rewritten
    let mut raised_tops: Option<Vec<crate::relief::Top>> = args
        .raise
        .as_ref()
        .filter(|r| r.values().any(|&n| n > 0))
        .map(|_| Vec::new());
    let mut done = 0;
    let empty: Vec<Seg> = Vec::new();
    for (k, (g, is_depth)) in all.iter().enumerate() {
        if !progress(k, all.len()) {
            break;
        }
        let g = g.as_slice();
        if *is_depth {
            let in_colour = colour_keys.contains(&key(g));
            let in_grout = grout_keys.contains(&key(g));
            // what else prints on this layer: from the solid infill's own parse,
            // where the solid infill isn't "other" (the top's parse counts it)
            let others = solid
                .as_ref()
                .and_then(|sp| sp.others.get(&g[0].layer))
                .unwrap_or(&empty);
            let c = colour.as_mut().filter(|_| in_colour);
            let cp = c.as_ref().map(|c| c.plan.checkpoint());
            // within the grout depth: the top's own tiles, grout and all;
            // else the colour layers' tiles (no grout)
            let (target, plan): (&mut PatternedAssembly, Option<&mut Plan>) = match c {
                Some(c) => {
                    let t = match (in_grout, c.depth.as_deref_mut()) {
                        (false, Some(d)) => d,
                        _ => &mut *pl,
                    };
                    (t, Some(&mut *c.plan))
                }
                None => (&mut *pl, None),
            };
            let what = match (in_colour, in_grout) {
                (true, true) => "colour depth and grout",
                (true, false) => "colour depth",
                _ => "grout",
            };
            match rewrite_group(g, args, others, None, target, plan, None) {
                Ok((rep, info)) => {
                    replacements.extend(rep);
                    log += &format!("layer {}: {} {}\n", g[0].layer, what, info);
                }
                Err(why) => {
                    if let (Some(c), Some(cp)) = (colour.as_mut(), cp) {
                        c.plan.rollback(cp);
                    }
                    let under = if in_colour {
                        "a coloured surface"
                    } else {
                        "a patterned surface's deeper grout"
                    };
                    log += &format!(
                        "layer {}: kept original solid infill under {} ({})\n",
                        g[0].layer, under, why
                    );
                }
            }
            continue;
        }
        let z = g[0].start.2;
        if let Some(mz) = args.max_z {
            if z > mz + 1e-6 {
                skipped += g.len();
                log += &format!(
                    "layer {}: kept original top surface (Z {:.2} above --max-z)\n",
                    g[0].layer, z
                );
                continue;
            }
        }
        if select.is_some_and(|f| !f(g)) {
            skipped += g.len();
            log += &format!(
                "layer {}: kept original top surface (not selected)\n",
                g[0].layer
            );
            continue;
        }
        let layer = g[0].layer;
        let raws =
            raws.get_or_insert_with(|| p.blocks.iter().map(|b| (b.layer, raw_of(b))).collect());
        let above: Vec<Polygon<f64>> = raws
            .iter()
            .filter(|(l, _)| *l > layer)
            .flat_map(|(_, r)| r.0.iter().cloned())
            .collect();
        let tc = std::time::Instant::now();
        let cover = if above.is_empty() {
            None
        } else {
            Some(geom::union_all(&above))
        };
        if std::env::var("PP_PROFILE").is_ok() {
            eprintln!("  cover {:.3}s (raws ready)", tc.elapsed().as_secs_f64());
        }
        let others = p.others.get(&layer).unwrap_or(&empty);
        let cp = colour.as_ref().map(|c| c.plan.checkpoint());
        let plan = colour.as_mut().map(|c| &mut *c.plan);
        match rewrite_group(
            g,
            args,
            others,
            cover.as_ref(),
            pl,
            plan,
            raised_tops.as_mut(),
        ) {
            Ok((rep, info)) => {
                done += g.len();
                replacements.extend(rep);
                log += &format!("layer {}: patterned {}\n", layer, info);
            }
            Err(why) => {
                if let (Some(c), Some(cp)) = (colour.as_mut(), cp) {
                    c.plan.rollback(cp);
                }
                skipped += g.len();
                log += &format!("layer {}: kept original top surface ({})\n", layer, why);
            }
        }
    }

    // the bottom surface, into the same replacements (its blocks index the same lines)
    let mut bottom_done = 0;
    let mut first_bottom: Option<usize> = None;
    let mut bottom_area: Vec<Polygon<f64>> = Vec::new();
    if let Some(b) = bottom {
        let mut groups: Vec<Vec<&Block>> = Vec::new();
        let mut keys: Vec<(usize, i64)> = Vec::new();
        for blk in &b.parsed.blocks {
            if let Some(why) = &blk.abort {
                log += &format!(
                    "layer {}: kept original bottom surface ({})\n",
                    blk.layer, why
                );
                skipped += 1;
                continue;
            }
            let key = (blk.layer, (blk.start.2 * 1000.0).round() as i64);
            match keys.iter().position(|k| *k == key) {
                Some(i) => groups[i].push(blk),
                None => {
                    keys.push(key);
                    groups.push(vec![blk]);
                }
            }
        }
        for g in groups {
            let layer = g[0].layer;
            let (lo, hi) = (g[0].first.unwrap_or(0), g[g.len() - 1].last);
            let why = if b.select.is_some_and(|f| !f(&g)) {
                Some("not selected".to_string())
            } else if replacements
                .iter()
                .any(|(&first, (last, _))| first <= hi && lo <= *last)
            {
                // a piece one layer thick: its top was patterned already
                Some("the same layer's top surface is patterned".to_string())
            } else {
                let others = b.parsed.others.get(&layer).unwrap_or(&empty);
                let cp = colour.as_ref().map(|c| c.plan.checkpoint());
                let plan = colour
                    .as_mut()
                    .map(|c| &mut *c.plan)
                    .filter(|p| p.uses_colour());
                match rewrite_group(&g, args, others, None, b.pl, plan, None) {
                    Ok((rep, info)) => {
                        bottom_done += g.len();
                        replacements.extend(rep);
                        log += &format!("layer {}: patterned bottom surface {}\n", layer, info);
                        first_bottom = Some(layer);
                        bottom_area.extend(g.iter().flat_map(|blk| raw_of(blk).0));
                        None
                    }
                    Err(why) => {
                        if let (Some(c), Some(cp)) = (colour.as_mut(), cp) {
                            c.plan.rollback(cp);
                        }
                        Some(why)
                    }
                }
            };
            if let Some(why) = why {
                skipped += g.len();
                log += &format!("layer {}: kept original bottom surface ({})\n", layer, why);
            }
        }
        // the layers above the first: within the relief depth (grout depth,
        // M22) the same tiles, lines and all (no grout: the face lies on the
        // plate); above that, within the bottom colour depth (M20b), the
        // slicer's lines recoloured
        let coloured = colour.as_ref().is_some_and(|c| c.plan.uses_colour());
        let relief = args.grout_depth.unwrap_or(1).max(1);
        let deep = relief.max(if coloured { b.depth } else { 1 });
        if let (true, Some(first)) = (deep > 1, first_bottom) {
            let region = geom::union_all(&bottom_area);
            let above = crate::gcode::find_blocks_of(
                p.lines.clone(),
                0.42,
                &["bottom surface", "solid infill", "internal solid infill"],
            );
            let mut groups: Vec<Vec<&Block>> = Vec::new();
            let mut keys: Vec<(usize, i64)> = Vec::new();
            for blk in &above.blocks {
                if blk.abort.is_some() || blk.layer <= first || blk.layer >= first + deep {
                    continue;
                }
                if geom::area(&geom::intersection(&raw_of(blk), &region)) < 1.0 {
                    continue;
                }
                let key = (blk.layer, (blk.start.2 * 1000.0).round() as i64);
                match keys.iter().position(|k| *k == key) {
                    Some(i) => groups[i].push(blk),
                    None => {
                        keys.push(key);
                        groups.push(vec![blk]);
                    }
                }
            }
            let mut found: Vec<usize> = Vec::new();
            for g in groups {
                let layer = g[0].layer;
                found.push(layer);
                let (lo, hi) = (g[0].first.unwrap_or(0), g[g.len() - 1].last);
                if replacements
                    .iter()
                    .any(|(&first, (last, _))| first <= hi && lo <= *last)
                {
                    continue;
                }
                if layer < first + relief {
                    let others = above.others.get(&layer).unwrap_or(&empty);
                    let cp = colour.as_ref().map(|c| c.plan.checkpoint());
                    let plan = colour
                        .as_mut()
                        .map(|c| &mut *c.plan)
                        .filter(|p| p.uses_colour());
                    match rewrite_group(&g, args, others, None, b.pl, plan, None) {
                        Ok((rep, info)) => {
                            replacements.extend(rep);
                            log += &format!("layer {}: bottom relief {}\n", layer, info);
                        }
                        Err(why) => {
                            if let (Some(c), Some(cp)) = (colour.as_mut(), cp) {
                                c.plan.rollback(cp);
                            }
                            log += &format!(
                                "layer {}: kept original solid layer over a patterned bottom ({})\n",
                                layer, why
                            );
                        }
                    }
                    continue;
                }
                let c = colour.as_mut().unwrap();
                let cp = c.plan.checkpoint();
                match recolour_group(&g, args, &region, b.pl, c.plan) {
                    Ok((rep, info)) => {
                        replacements.extend(rep);
                        log += &format!("layer {}: bottom colour depth {}\n", layer, info);
                    }
                    Err(why) => {
                        c.plan.rollback(cp);
                        log += &format!(
                            "layer {}: kept original solid layer over a coloured bottom ({})\n",
                            layer, why
                        );
                    }
                }
            }
            // relief deeper than the bottom's solid layers: say where it stops
            if let Some(l) = (first + 1..first + relief).find(|l| !found.contains(l)) {
                log += &format!(
                    "depth: layer {l} over the bottom isn't solid; the bottom pattern stops there\n"
                );
            }
        }
    }

    // raised tiles: the new layers over the patterned tops
    let mut added = 0;
    if let (Some(tops), Some(raise)) = (raised_tops.take(), args.raise.as_ref()) {
        let mut thin = 0;
        let mut levels = Vec::new();
        for t in &tops {
            levels.extend(crate::relief::levels(pl, t, raise, &mut thin));
        }
        if thin > 0 {
            log += &format!(
                "relief: {thin} tile(s) too narrow to raise (under two line widths) left flat\n"
            );
        }
        match crate::relief::insert(&p.lines, levels, args, &replacements) {
            Ok((rep, n, _, rlog)) => {
                replacements.extend(rep);
                added = n;
                log += &rlog;
            }
            Err(why) => log += &format!("relief: raised tiles left out ({why})\n"),
        }
    }

    let lines = &p.lines;
    let note = if bottom_done > 0 {
        format!(
            "; postprocessed by TessaFab 1.2.0: {} top surface block(s) rewritten, {} bottom, {} kept\n",
            done, bottom_done, skipped
        )
    } else {
        format!(
            "; postprocessed by TessaFab 1.2.0: {} top surface block(s) rewritten, {} kept\n",
            done, skipped
        )
    };
    let done = done + bottom_done;
    let header_end = lines.iter().take(400).position(|l| {
        l.trim()
            .to_ascii_uppercase()
            .starts_with("; HEADER_BLOCK_END")
    });
    let mut out = Vec::with_capacity(lines.len() + 10000);
    if header_end.is_none() {
        out.push(note.clone());
    }
    let mut i = 0;
    while i < lines.len() {
        if let Some((last, new)) = replacements.get(&i) {
            out.extend(new.iter().cloned());
            i = last + 1;
            continue;
        }
        out.push(lines[i].clone());
        if Some(i) == header_end {
            out.push(note.clone());
        }
        i += 1;
    }
    let _: Option<Coord<f64>> = None;
    (out, done, skipped, log, added)
}

#[cfg(test)]
mod emitter_tests {
    use super::*;

    #[test]
    fn finish_restores_a_fractional_feed_rate() {
        let args = crate::args::RunArgs::default().engine_args();
        let mut em = Emitter::new((0.0, 0.0, 0.2, 0.0, 1200.0), false, &args);
        em.finish((0.0, 0.0, 0.2, 0.0, 9466.211));
        assert_eq!(em.out.last().unwrap(), "G1 F9466.211\n");
        let mut em = Emitter::new((0.0, 0.0, 0.2, 0.0, 1200.0), false, &args);
        em.finish((0.0, 0.0, 0.2, 0.0, 9000.0));
        assert_eq!(em.out.last().unwrap(), "G1 F9000\n");
    }
}
