//! Geometry helpers over `geo` (booleans, offsets and line clipping via i_overlay),
//! shaped like the shapely calls the Python engine makes.

use geo::algorithm::buffer::{BufferStyle, LineCap, LineJoin};
use geo::{
    Area, BooleanOps, BoundingRect, Buffer, Coord, InteriorPoint, Intersects, LineString,
    MultiLineString, MultiPolygon, Point, Polygon, Rect, Simplify,
};
use i_overlay::core::overlay_rule::OverlayRule;

pub type MP = MultiPolygon<f64>;

/// shapely's default mitre limit (5) as i_overlay's minimum sharp angle: 2 asin(1/5).
const MITRE_ANGLE: f64 = 0.4027158415806615;
/// shapely's 16 segments per quarter circle: L / R = pi / 32.
const ROUND_STEP: f64 = std::f64::consts::PI / 32.0;

pub fn empty() -> MP {
    MultiPolygon::new(vec![])
}

pub fn is_empty(g: &MP) -> bool {
    g.0.is_empty()
}

pub fn area(g: &MP) -> f64 {
    g.unsigned_area()
}

/// Offset a region: mitred (shapely join_style=2) or round (join_style=1) corners.
/// Edges shorter than this (mm) are dropped before offsetting.
const TINY_EDGE: f64 = 1e-6;

/// The ring without edges shorter than TINY_EDGE (still closed). The offsetter
/// takes each edge's direction from its end points, so on an edge of 1e-8 mm
/// (float noise, which i_overlay's own offsets leave behind) the direction is
/// noise too, and offsetting such a shape again bent it: concentric rings of
/// a finely drawn tile came out up to 2% short and the innermost ones were
/// lost.
fn drop_tiny_edges(r: &LineString<f64>) -> LineString<f64> {
    let mut out: Vec<Coord<f64>> = Vec::with_capacity(r.0.len());
    for c in &r.0 {
        if out
            .last()
            .is_none_or(|l: &Coord<f64>| (c.x - l.x).hypot(c.y - l.y) >= TINY_EDGE)
        {
            out.push(*c);
        }
    }
    // the closing point may now sit a hair off the first: close exactly
    if out.len() > 1 {
        let (f, l) = (out[0], out[out.len() - 1]);
        if (f.x - l.x).hypot(f.y - l.y) < TINY_EDGE {
            out.pop();
        }
        out.push(f);
    }
    if out.len() < 4 {
        return r.clone();
    }
    LineString::new(out)
}

fn without_tiny_edges(g: &MP) -> MP {
    MultiPolygon::new(
        g.iter()
            .map(|p| {
                Polygon::new(
                    drop_tiny_edges(p.exterior()),
                    p.interiors().iter().map(drop_tiny_edges).collect(),
                )
            })
            .collect(),
    )
}

pub fn buffer(g: &MP, d: f64, round: bool) -> MP {
    if d == 0.0 || is_empty(g) {
        return g.clone();
    }
    // GEOS (shapely) first simplifies the input to 1% of the distance
    // (BufferInputLineSimplifier); without it a curve traced at the slicer's
    // resolution kept every 0.035 mm point through every rim and ring. GEOS
    // only removes points on the side that can't change the result; this
    // simplification is two-sided, so it stays within 2 microns.
    let g = &simplify(&without_tiny_edges(g), (d.abs() * 0.01).min(0.002));
    let offset = |g: &MP| {
        if round {
            g.buffer_with_style(BufferStyle::new(d).line_join(LineJoin::Round(ROUND_STEP)))
        } else {
            merge_bevels(
                g.buffer_with_style(BufferStyle::new(d).line_join(LineJoin::Miter(MITRE_ANGLE))),
                d,
            )
        }
    };
    let out = offset(g);
    // Offsetting silently gives too little on rings that cross themselves; an
    // outward offset can only grow a valid shape, so if it shrank, clean and retry.
    if d > 0.0 && area(&out) + 1e-9 < area(g) {
        return offset(&union_rings(g.iter()));
    }
    out
}

/// i_overlay bevels (two vertices) every turn under 5 degrees, even with a
/// mitre join, so each offset of a smooth curve doubled its vertices (an
/// inset of a 149-point curve had 291) and rims and rings along curved edges
/// came out as runs of 0.02 mm moves. Each such bevel edge (shorter than a
/// 5-degree bevel at this offset) is put back to the mitre point, where the
/// edges either side of it meet, as GEOS makes it.
fn merge_bevels(g: MP, d: f64) -> MP {
    let max_len = 2.0 * d.abs() * (2.5f64).to_radians().tan() * 1.01;
    let fix = |r: &LineString<f64>| -> LineString<f64> {
        let mut pts: Vec<Coord<f64>> = r.0.clone();
        pts.pop(); // closing point
        if pts.len() < 5 {
            return r.clone();
        }
        let mut out: Vec<Coord<f64>> = Vec::with_capacity(pts.len());
        let n = pts.len();
        let mut skip_first = false;
        let mut i = 0;
        while i < n {
            let (b, c) = (pts[i], pts[(i + 1) % n]);
            let len = (c.x - b.x).hypot(c.y - b.y);
            if len < max_len && len > 0.0 {
                let (a, e) = (pts[(i + n - 1) % n], pts[(i + 2) % n]);
                // where line a-b meets line c-e
                let (d1x, d1y, d2x, d2y) = (b.x - a.x, b.y - a.y, e.x - c.x, e.y - c.y);
                let den = d1x * d2y - d1y * d2x;
                let p = if den.abs() > 1e-15 {
                    let t = ((c.x - b.x) * d2y - (c.y - b.y) * d2x) / den;
                    Coord {
                        x: b.x + t * d1x,
                        y: b.y + t * d1y,
                    }
                } else {
                    Coord {
                        x: (b.x + c.x) / 2.0,
                        y: (b.y + c.y) / 2.0,
                    }
                };
                // only a bevel: the edges either side turn by under 5 degrees
                // (a short real edge, as on a small hexagon deep inside a
                // tile, turns far more) and the mitre point stays close
                let turn = (d1x * d2y - d1y * d2x).atan2(d1x * d2x + d1y * d2y).abs();
                if turn < (5.5f64).to_radians()
                    && (p.x - b.x).hypot(p.y - b.y) <= len
                    && (p.x - c.x).hypot(p.y - c.y) <= len
                {
                    out.push(p);
                    if i + 1 == n {
                        skip_first = true;
                    }
                    i += 2;
                    continue;
                }
            }
            out.push(b);
            i += 1;
        }
        if skip_first {
            out.remove(0);
        }
        if out.len() < 3 {
            return r.clone();
        }
        out.push(out[0]);
        LineString::new(out)
    };
    MultiPolygon::new(
        g.iter()
            .map(|p| Polygon::new(fix(p.exterior()), p.interiors().iter().map(fix).collect()))
            .collect(),
    )
}

/// The stroke of a closed path (`half` each side, mitred joins): a ring
/// with a join at its start like every other corner, as GEOS buffers a
/// LineString whose ends meet. geo's line buffer always strokes an open path,
/// so a closed one got two square caps at its first point instead, and those
/// cut small notches into the tiles beside it.
pub fn stroke_ring(pts: &[(f64, f64)], half: f64) -> MP {
    use i_overlay::mesh::float::stroke::offset::StrokeOffset;
    use i_overlay::mesh::float::style::{LineJoin as Join, StrokeStyle};
    let mut path: Vec<[f64; 2]> = pts.iter().map(|p| [p.0, p.1]).collect();
    while path.len() > 1 && path.first() == path.last() {
        path.pop();
    }
    if path.len() < 2 || half <= 0.0 {
        return empty();
    }
    let style = StrokeStyle::new(2.0 * half).line_join(Join::Miter(MITRE_ANGLE));
    from_shapes(path.stroke(style, true))
}

pub fn poly_mp(p: &Polygon<f64>) -> MP {
    MultiPolygon::new(vec![p.clone()])
}

pub fn union(a: &MP, b: &MP) -> MP {
    if is_empty(a) {
        return b.clone();
    }
    if is_empty(b) {
        return a.clone();
    }
    overlay(a, b, OverlayRule::Union)
}

pub fn difference(a: &MP, b: &MP) -> MP {
    if is_empty(a) || is_empty(b) {
        return a.clone();
    }
    overlay(a, b, OverlayRule::Difference)
}

pub fn intersection(a: &MP, b: &MP) -> MP {
    if is_empty(a) || is_empty(b) {
        return empty();
    }
    overlay(a, b, OverlayRule::Intersect)
}

/// Symmetric difference (even-odd).
pub fn xor(a: &MP, b: &MP) -> MP {
    overlay(a, b, OverlayRule::Xor)
}

pub fn union_all(ps: &[Polygon<f64>]) -> MP {
    if ps.is_empty() {
        return empty();
    }
    union_rings(ps.iter())
}

/// The union of polygons, each read by the non-zero rule after orienting it
/// (outline counter-clockwise, holes clockwise), so overlaps merge whatever
/// way the rings ran. Rings that cross themselves come out cleaned (like
/// shapely's buffer(0)).
pub fn union_rings<'a>(ps: impl IntoIterator<Item = &'a Polygon<f64>>) -> MP {
    use i_overlay::core::fill_rule::FillRule;
    use i_overlay::float::overlay::{FloatOverlay, OverlayOptions};
    let shapes: Vec<Vec<Vec<[f64; 2]>>> = ps
        .into_iter()
        .flat_map(|p| to_shapes(&MultiPolygon::new(vec![ccw(p)])))
        .collect();
    if shapes.is_empty() {
        return empty();
    }
    from_shapes(
        FloatOverlay::with_subj_custom(&shapes, OverlayOptions::ogc(), Default::default())
            .overlay(OverlayRule::Subject, FillRule::NonZero),
    )
}

fn ccw(p: &Polygon<f64>) -> Polygon<f64> {
    use geo::orient::{Direction, Orient};
    p.orient(Direction::Default)
}

/// A segment's footprint with square caps (shapely cap_style=3): a rectangle.
pub fn seg_rect(x0: f64, y0: f64, x1: f64, y1: f64, w: f64) -> Polygon<f64> {
    let len = (x1 - x0).hypot(y1 - y0);
    let (ux, uy) = ((x1 - x0) / len, (y1 - y0) / len);
    let h = w / 2.0;
    let (vx, vy) = (-uy * h, ux * h);
    let (ax, ay) = (x0 - ux * h, y0 - uy * h);
    let (bx, by) = (x1 + ux * h, y1 + uy * h);
    Polygon::new(
        LineString::from(vec![
            (ax - vx, ay - vy),
            (bx - vx, by - vy),
            (bx + vx, by + vy),
            (ax + vx, ay + vy),
            (ax - vx, ay - vy),
        ]),
        vec![],
    )
}

/// A segment's footprint with round caps (shapely cap_style=1).
pub fn capsule(x0: f64, y0: f64, x1: f64, y1: f64, r: f64) -> Polygon<f64> {
    let a = (y1 - y0).atan2(x1 - x0);
    let n = 16;
    let mut pts = Vec::with_capacity(2 * n + 3);
    let half = std::f64::consts::PI / 2.0;
    for k in 0..=n {
        let t = a - half + std::f64::consts::PI * k as f64 / n as f64;
        pts.push((x1 + r * t.cos(), y1 + r * t.sin()));
    }
    for k in 0..=n {
        let t = a + half + std::f64::consts::PI * k as f64 / n as f64;
        pts.push((x0 + r * t.cos(), y0 + r * t.sin()));
    }
    pts.push(pts[0]);
    Polygon::new(LineString::from(pts), vec![])
}

pub fn bbox(g: &MP) -> Option<Rect<f64>> {
    g.bounding_rect()
}

pub fn bbox_hit(a: &Rect<f64>, b: &Rect<f64>) -> bool {
    a.min().x <= b.max().x
        && b.min().x <= a.max().x
        && a.min().y <= b.max().y
        && b.min().y <= a.max().y
}

pub fn contains_pt(g: &MP, x: f64, y: f64) -> bool {
    g.intersects(&Point::new(x, y))
}

pub fn interior_pt(p: &Polygon<f64>) -> Option<(f64, f64)> {
    p.interior_point().map(|q| (q.x(), q.y()))
}

/// Douglas-Peucker simplification that keeps every polygon valid (like
/// shapely's preserve_topology=True): a polygon whose simplified outline
/// would cross itself or a hole keeps its original rings.
pub fn simplify(g: &MP, eps: f64) -> MP {
    use geo::Validation;
    MultiPolygon::new(
        g.iter()
            .map(|p| {
                let q = p.simplify(eps);
                if q.exterior().0.len() >= 4 && q.is_valid() {
                    q
                } else {
                    p.clone()
                }
            })
            .collect(),
    )
}

/// The part of the lines inside `region` (boundary included).
pub fn clip_lines(region: &MP, lines: &[LineString<f64>]) -> Vec<LineString<f64>> {
    if is_empty(region) || lines.is_empty() {
        return vec![];
    }
    region.clip(&MultiLineString::new(lines.to_vec()), false).0
}

/// The part of the lines outside `region`.
pub fn clip_out(region: &MP, lines: &[LineString<f64>]) -> Vec<LineString<f64>> {
    if is_empty(region) {
        return lines.to_vec();
    }
    region.clip(&MultiLineString::new(lines.to_vec()), true).0
}

pub fn rings(g: &MP) -> Vec<LineString<f64>> {
    let mut out = Vec::new();
    for p in g.iter() {
        out.push(p.exterior().clone());
        out.extend(p.interiors().iter().cloned());
    }
    out
}

pub fn length(l: &LineString<f64>) -> f64 {
    l.0.windows(2)
        .map(|w| (w[1].x - w[0].x).hypot(w[1].y - w[0].y))
        .sum()
}

fn close(a: Coord<f64>, b: Coord<f64>) -> bool {
    (a.x - b.x).abs() < 1e-9 && (a.y - b.y).abs() < 1e-9
}

/// Join line pieces that meet end to end (shapely's linemerge, enough for ring pieces).
pub fn linemerge(mut parts: Vec<LineString<f64>>) -> Vec<LineString<f64>> {
    parts.retain(|l| l.0.len() >= 2);
    let mut changed = true;
    while changed && parts.len() > 1 {
        changed = false;
        'outer: for i in 0..parts.len() {
            for j in 0..parts.len() {
                if i == j {
                    continue;
                }
                let (ai, aj) = (parts[i].0.clone(), parts[j].0.clone());
                let joined: Option<Vec<Coord<f64>>> = if close(*ai.last().unwrap(), aj[0]) {
                    Some(ai.iter().chain(aj[1..].iter()).cloned().collect())
                } else if close(*ai.last().unwrap(), *aj.last().unwrap()) {
                    Some(ai.iter().chain(aj.iter().rev().skip(1)).cloned().collect())
                } else {
                    None
                };
                if let Some(c) = joined {
                    // only where exactly two pieces meet (degree 2), like linemerge
                    let end = *ai.last().unwrap();
                    let degree = parts
                        .iter()
                        .filter(|p| close(p.0[0], end) || close(*p.0.last().unwrap(), end))
                        .count();
                    if degree != 2 {
                        continue;
                    }
                    let (hi, lo) = if i > j { (i, j) } else { (j, i) };
                    parts.remove(hi);
                    parts.remove(lo);
                    parts.push(LineString::new(c));
                    changed = true;
                    break 'outer;
                }
            }
        }
    }
    parts
}

pub fn polys_area_over(g: MP, min: f64) -> MP {
    MultiPolygon::new(
        g.0.into_iter()
            .filter(|p| p.unsigned_area() > min)
            .collect(),
    )
}

pub fn exterior_only(p: &Polygon<f64>) -> Polygon<f64> {
    Polygon::new(p.exterior().clone(), vec![])
}

/// Union of round-capped segment footprints (x0, y0, x1, y1, radius): consecutive
/// segments with the same radius are chained into polylines and stroked once, which
/// is far cheaper than unioning one capsule per segment.
pub fn stroke_segments(segs: &[(f64, f64, f64, f64, f64)]) -> MP {
    let mut groups: Vec<(f64, Vec<LineString<f64>>)> = Vec::new();
    let mut cur: Vec<(f64, f64)> = Vec::new();
    let mut cur_r = f64::NAN;
    let flush =
        |cur: &mut Vec<(f64, f64)>, r: f64, groups: &mut Vec<(f64, Vec<LineString<f64>>)>| {
            if cur.len() >= 2 {
                let ls = LineString::from(std::mem::take(cur));
                match groups.iter_mut().find(|g| (g.0 - r).abs() < 1e-9) {
                    Some(g) => g.1.push(ls),
                    None => groups.push((r, vec![ls])),
                }
            }
            cur.clear();
        };
    for &(x0, y0, x1, y1, r) in segs {
        let joins = !cur.is_empty() && (r - cur_r).abs() < 1e-9 && {
            let (lx, ly) = *cur.last().unwrap();
            (lx - x0).abs() < 1e-9 && (ly - y0).abs() < 1e-9
        };
        if !joins {
            flush(&mut cur, cur_r, &mut groups);
            cur.push((x0, y0));
            cur_r = r;
        }
        cur.push((x1, y1));
    }
    flush(&mut cur, cur_r, &mut groups);
    let mut out = empty();
    for (r, lines) in groups {
        let style = BufferStyle::new(r)
            .line_join(LineJoin::Round(ROUND_STEP))
            .line_cap(LineCap::Round(ROUND_STEP));
        let g = MultiLineString::new(lines).buffer_with_style(style);
        out = union(&out, &g);
    }
    out
}

fn to_shapes(g: &MP) -> Vec<Vec<Vec<[f64; 2]>>> {
    g.iter()
        .map(|p| {
            std::iter::once(p.exterior())
                .chain(p.interiors())
                .map(|r| {
                    let mut v: Vec<[f64; 2]> = r.0.iter().map(|c| [c.x, c.y]).collect();
                    v.pop(); // i_overlay paths are implicitly closed
                    v
                })
                .collect()
        })
        .collect()
}

pub(crate) fn from_shapes(shapes: Vec<Vec<Vec<[f64; 2]>>>) -> MP {
    MultiPolygon::new(
        shapes
            .into_iter()
            .filter(|s| !s.is_empty())
            .map(|s| {
                let mut rings = s.into_iter().map(|c| {
                    let mut pts: Vec<(f64, f64)> = c.into_iter().map(|q| (q[0], q[1])).collect();
                    pts.push(pts[0]);
                    LineString::from(pts)
                });
                let ext = rings.next().unwrap();
                Polygon::new(ext, rings.collect())
            })
            .collect(),
    )
}

/// A boolean operation through i_overlay, with OGC-valid output (no ring
/// touching itself: the offsetting code silently fails on such rings). Called
/// directly: geo's BooleanOps wrapper takes seconds where this takes
/// milliseconds on a plate's walls.
pub fn overlay(a: &MP, b: &MP, rule: OverlayRule) -> MP {
    use i_overlay::core::fill_rule::FillRule;
    use i_overlay::float::overlay::{FloatOverlay, OverlayOptions};
    let (sa, sb) = (to_shapes(a), to_shapes(b));
    from_shapes(
        FloatOverlay::with_subj_and_clip_custom(
            &sa,
            &sb,
            OverlayOptions::ogc(),
            Default::default(),
        )
        .overlay(rule, FillRule::EvenOdd),
    )
}

/// A point inside a polygon, as GEOS (shapely's representative_point) finds
/// it: on a horizontal scan line through the middle of the polygon, at the
/// centre of the widest stretch inside. The engines test one such point per
/// hole, so they must pick the same one.
pub fn representative_point(p: &Polygon<f64>) -> Option<(f64, f64)> {
    let rings: Vec<&LineString<f64>> = std::iter::once(p.exterior()).chain(p.interiors()).collect();
    let b = p.bounding_rect()?;
    let centre = (b.min().y + b.max().y) / 2.0;
    let (mut hi, mut lo) = (b.max().y, b.min().y);
    for r in &rings {
        for c in &r.0 {
            if c.y <= centre {
                if c.y > lo {
                    lo = c.y;
                }
            } else if c.y < hi {
                hi = c.y;
            }
        }
    }
    let scan = (hi + lo) / 2.0;
    let mut xs: Vec<f64> = Vec::new();
    for r in &rings {
        for w in r.0.windows(2) {
            let (p0, p1) = (w[0], w[1]);
            let (y0, y1) = (p0.y, p1.y);
            if (y0 > scan && y1 > scan) || (y0 < scan && y1 < scan) || y0 == y1 {
                continue;
            }
            // vertices on the scan line: a downward edge doesn't count its start, an upward one its end
            if (y0 == scan && y1 < scan) || (y1 == scan && y0 < scan) {
                continue;
            }
            xs.push(p0.x + (scan - y0) * (p1.x - p0.x) / (y1 - y0));
        }
    }
    xs.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let mut best: Option<(f64, f64)> = None;
    let mut width = -1.0;
    for pair in xs.chunks_exact(2) {
        let w = pair[1] - pair[0];
        if w > width {
            width = w;
            best = Some(((pair[0] + pair[1]) / 2.0, scan));
        }
    }
    best.or_else(|| interior_pt(p))
}

#[cfg(test)]
mod closing_tests {
    use super::*;

    /// A 20 mm square with a round hole (64-gon) of radius `r`, closed by `c` with mitred joins.
    fn hole_after_closing(r: f64, c: f64, round: bool) -> f64 {
        let ring: Vec<(f64, f64)> = (0..=64)
            .map(|k| {
                let a = -(k as f64) / 64.0 * std::f64::consts::TAU; // hole: clockwise
                (10.0 + r * a.cos(), 10.0 + r * a.sin())
            })
            .collect();
        let sq = LineString::from(vec![
            (0.0, 0.0),
            (20.0, 0.0),
            (20.0, 20.0),
            (0.0, 20.0),
            (0.0, 0.0),
        ]);
        let g = MultiPolygon::new(vec![Polygon::new(sq, vec![LineString::from(ring)])]);
        let closed = buffer(&buffer(&g, c, round), -c, round);
        400.0 - area(&closed)
    }

    #[test]
    fn closing_keeps_a_hole_wider_than_twice_the_radius() {
        let r = 1.53;
        let hole = std::f64::consts::PI * r * r;
        for c in [1.03, 1.18, 1.27] {
            let mitre = hole_after_closing(r, c, false);
            let round = hole_after_closing(r, c, true);
            eprintln!("r {r} c {c}: hole {hole:.3} -> mitre {mitre:.3}, round {round:.3}");
        }
        assert!(hole_after_closing(r, 1.03, false) > 0.5 * hole);
    }
}

#[cfg(test)]
mod simplify_tests {
    use super::*;

    #[test]
    fn simplifying_never_makes_a_polygon_cross_itself() {
        // a thin zig-zag notch: plain Douglas-Peucker at 0.5 cuts across it
        let ring = LineString::from(vec![
            (0.0, 0.0),
            (10.0, 0.0),
            (10.0, 10.0),
            (5.2, 10.0),
            (5.0, 0.3),
            (4.8, 10.0),
            (0.0, 10.0),
            (0.0, 0.0),
        ]);
        let g = MultiPolygon::new(vec![Polygon::new(ring, vec![])]);
        let s = simplify(&g, 0.5);
        use geo::Validation;
        assert!(s.0.iter().all(|p| p.is_valid()));
        assert!((area(&s) - area(&g)).abs() < 1.0);
    }
}

#[cfg(test)]
mod offset_tests {
    use super::*;

    #[test]
    fn offsetting_an_offset_matches_shapely() {
        // an ogee tile as the Rust compose draws it (finely): its inset has
        // 1e-8 mm edges; the rings inside it must match shapely's
        let pts: Vec<(f64, f64)> =
            serde_json::from_str::<Vec<(f64, f64)>>(include_str!("../tests/data/ogee_tile.json"))
                .unwrap();
        let tile = MultiPolygon::new(vec![Polygon::new(LineString::from(pts), vec![])]);
        let s = 0.42 - 0.2 * (1.0 - std::f64::consts::PI / 4.0);
        let inset = buffer(&tile, -s / 2.0, false);
        // shapely: frame.buffer(-k * s, join_style=2).area
        let shapely = [(3, 79.93), (4, 67.79), (8, 28.73), (14, 0.28)];
        // one vertex per vertex, as GEOS: no bevel pairs at the small turns of a curve
        let n = |g: &MP| g.0.iter().map(|p| p.exterior().0.len()).sum::<usize>();
        assert!(
            n(&inset) <= n(&tile),
            "inset has {} vertices, the tile {}",
            n(&inset),
            n(&tile)
        );
        for (k, want) in shapely {
            let got = area(&buffer(&inset, -(k as f64) * s, false));
            assert!(
                (got - want).abs() < 0.01,
                "ring {k}: {got:.3} vs shapely {want}"
            );
        }
    }

    #[test]
    fn a_deep_inset_of_a_hexagon_stays_a_hexagon() {
        // a 5 mm hexagon inset by 4.1 mm: its 0.27 mm edges are real edges,
        // not bevels (they turn 60 degrees), and all six stay
        let r = 5.0;
        let hex: Vec<(f64, f64)> = (0..=6)
            .map(|k| {
                let a = (k % 6) as f64 * std::f64::consts::PI / 3.0;
                (r * a.cos(), r * a.sin())
            })
            .collect();
        let g = MultiPolygon::new(vec![Polygon::new(LineString::from(hex), vec![])]);
        let d = 4.1;
        let inset = buffer(&g, -d, false);
        let ring = inset.0[0].exterior();
        assert_eq!(ring.0.len(), 7, "{:?}", ring.0);
        // a regular hexagon of inradius r cos 30 - d
        let side = (r * 30f64.to_radians().cos() - d) * 2.0 / 3f64.sqrt();
        assert!((length(ring) - 6.0 * side).abs() < 1e-6, "{}", length(ring));
    }
}
