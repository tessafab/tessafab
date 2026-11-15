//! A tile drawing: the tiles, grout, piece outlines and red lines of a pattern
//! drawing in mm, and composing one pattern repeat over a layout of pieces.
//! A port of TileDrawing and compose() in tilepattern.py.
//!
//! In a drawing, any fill is a tile; a blue stroke is grout (a groove as wide
//! as the stroke); a light red line in a tile sets the direction of its lines
//! (an open line) or makes it concentric rings (a closed loop), for every tile
//! of that fill colour; a black stroke with no fill is a printed piece's outline.

use crate::direction::Direction;
use crate::svg::{self, color_key, stroke_role, Drawing, Item, Paint, Role, Subpath};
use geo::algorithm::buffer::{BufferStyle, LineCap, LineJoin};
use geo::{
    Area, BooleanOps, BoundingRect, Buffer, Centroid, Contains, Coord, LineString,
    MinimumRotatedRect, MultiLineString, MultiPolygon, Point, Polygon, Rect,
};
use std::collections::BTreeMap;

pub type MP = MultiPolygon<f64>;

/// shapely's default mitre limit (5) as i_overlay's minimum sharp angle: 2 asin(1/5).
const MITRE_ANGLE: f64 = 0.4027158415806615;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// A red line in this tile.
    OwnLine,
    /// Another tile of the same fill colour has a red line.
    Colour,
    /// No red line in its colour: the default angle or the tile's long axis.
    Default,
}

#[derive(Debug, Clone)]
pub struct Tile {
    pub poly: Polygon<f64>,
    pub bbox: Rect<f64>,
    /// How the tile's lines run (an angle in degrees, or concentric rings).
    pub direction: Direction,
    /// The tile group: its fill colour (`#rrggbb`).
    pub color: String,
    pub source: Source,
}

#[derive(Debug, Clone)]
pub struct TileDrawing {
    pub tiles: Vec<Tile>,
    pub grout: MP,
    pub outlines: Vec<Polygon<f64>>,
    /// Red lines as drawn: points and closed flag.
    pub reds: Vec<Subpath>,
    pub red_lines: Vec<LineString<f64>>,
    /// Literal mode: tiles that red lines run through.
    pub red_tiles: Vec<usize>,
    /// Direction per fill colour, from its red lines.
    pub color_rule: BTreeMap<String, Direction>,
    pub literal: bool,
    pub default_angle: Option<f64>,
    pub defaulted: usize,
    /// Strokes with no meaning: description -> count, in first-seen order.
    pub ignored: Vec<(String, usize)>,
    pub size: (f64, f64),
}

fn polygon(pts: &[(f64, f64)]) -> Polygon<f64> {
    let mut ring: Vec<(f64, f64)> = pts.to_vec();
    if ring.first() != ring.last() {
        ring.push(ring[0]);
    }
    Polygon::new(LineString::from(ring), vec![])
}

fn union_of<'a>(ps: impl IntoIterator<Item = &'a Polygon<f64>>) -> MP {
    crate::geom::union_rings(ps)
}

/// Valid polygons from a ring that may cross itself (shapely's buffer(0)):
/// everything the ring winds round (non-zero rule), whichever way it runs.
fn clean(p: Polygon<f64>) -> MP {
    if p.exterior().0.len() < 4 {
        return MultiPolygon::new(vec![]);
    }
    crate::geom::union_rings([&p])
}

/// A layout's piece outlines (unfilled shapes with a black stroke) for
/// matching pieces to: each sub-path made valid and kept whole if it holds
/// more than 1 mm2, as assembly.import_layout reads them.
pub fn layout_pieces(items: &[Item]) -> Vec<MP> {
    let mut out = Vec::new();
    for it in items {
        if it.fill.is_some() || stroke_role(it.stroke) != Some(Role::Outline) {
            continue;
        }
        for (pts, _) in &it.subpaths {
            if pts.len() >= 3 {
                let p = clean(polygon(pts));
                if p.unsigned_area() > 1.0 {
                    out.push(p);
                }
            }
        }
    }
    out
}

fn bbox_hit(a: &Rect<f64>, b: &Rect<f64>) -> bool {
    a.min().x <= b.max().x
        && b.min().x <= a.max().x
        && a.min().y <= b.max().y
        && b.min().y <= a.max().y
}

/// Fill area of an item's sub-paths (even-odd, every sub-path closed).
fn area_of(subpaths: &[Subpath]) -> MP {
    let mut g: MP = MultiPolygon::new(vec![]);
    for (pts, _) in subpaths {
        if pts.len() < 3 {
            continue;
        }
        let p = clean(polygon(pts));
        if p.0.is_empty() {
            continue;
        }
        g = if g.0.is_empty() {
            p
        } else {
            crate::geom::xor(&g, &p)
        };
    }
    g
}

/// Direction (degrees, 0..180) of an open red line: first to last point.
fn direction(pts: &[(f64, f64)]) -> f64 {
    let (mut dx, mut dy) = (
        pts[pts.len() - 1].0 - pts[0].0,
        pts[pts.len() - 1].1 - pts[0].1,
    );
    if dx.hypot(dy) < 1e-6 {
        // fall back to the longest segment
        let mut best = 0;
        let mut best_len = -1.0;
        for i in 0..pts.len() - 1 {
            let l = (pts[i + 1].0 - pts[i].0).hypot(pts[i + 1].1 - pts[i].1);
            if l > best_len {
                best_len = l;
                best = i;
            }
        }
        dx = pts[best + 1].0 - pts[best].0;
        dy = pts[best + 1].1 - pts[best].1;
    }
    dy.atan2(dx).to_degrees().rem_euclid(180.0)
}

/// Direction of a tile's long axis (its minimum rotated rectangle's longest side).
fn long_axis(p: &Polygon<f64>) -> f64 {
    let Some(rect) = p.minimum_rotated_rect() else {
        return 0.0;
    };
    let cs = &rect.exterior().0;
    if cs.len() < 4 {
        return 0.0;
    }
    let mut best = (0usize, -1.0);
    for k in 0..4 {
        let (a, b) = (cs[k], cs[(k + 1) % 4]);
        let l = (b.x - a.x).hypot(b.y - a.y);
        if l > best.1 {
            best = (k, l);
        }
    }
    let (a, b) = (cs[best.0], cs[(best.0 + 1) % 4]);
    (b.y - a.y).atan2(b.x - a.x).to_degrees().rem_euclid(180.0)
}

/// A point on a line (GEOS's interior point of a line: the inner vertex nearest
/// the centroid, else the nearer end).
fn point_on_line(l: &LineString<f64>) -> Option<Point<f64>> {
    let c = l.centroid()?;
    let pts = &l.0;
    let pick = |range: &[Coord<f64>]| -> Option<Coord<f64>> {
        range
            .iter()
            .min_by(|a, b| {
                let da = (a.x - c.x()).hypot(a.y - c.y());
                let db = (b.x - c.x()).hypot(b.y - c.y());
                da.partial_cmp(&db).unwrap()
            })
            .copied()
    };
    let inner = if pts.len() > 2 {
        pick(&pts[1..pts.len() - 1])
    } else {
        None
    };
    inner
        .or_else(|| pick(&[pts[0], pts[pts.len() - 1]]))
        .map(|p| Point::new(p.x, p.y))
}

fn clip_length(poly: &Polygon<f64>, line: &LineString<f64>) -> f64 {
    use geo::Length;
    let ml = MultiLineString::new(vec![line.clone()]);
    poly.clip(&ml, false)
        .0
        .iter()
        .map(|l| geo::Euclidean.length(l))
        .sum()
}

fn grout_shape(pts: &[(f64, f64)], closed: bool, half: f64) -> MP {
    // closed, or ending where it starts: a ring (GEOS buffers both alike)
    if pts.len() > 2 && (closed || pts.first() == pts.last()) {
        return crate::geom::stroke_ring(pts, half);
    }
    let line = LineString::from(pts.to_vec());
    let style = BufferStyle::new(half)
        .line_cap(LineCap::Square)
        .line_join(LineJoin::Miter(MITRE_ANGLE));
    line.buffer_with_style(style)
}

fn round_half_even(x: f64) -> f64 {
    if (x - x.trunc()).abs() == 0.5 {
        2.0 * (x / 2.0).round()
    } else {
        x.round()
    }
}

/// IEEE remainder (Python's math.remainder): quotient rounded half to even.
fn ieee_remainder(x: f64, y: f64) -> f64 {
    x - y * round_half_even(x / y)
}

impl TileDrawing {
    /// Tiles from a drawing's items (`default_angle`: for tiles whose colour
    /// has no red line; else their long axis; `literal`: red lines are paths to print).
    pub fn from_items(
        items: &[Item],
        size: (f64, f64),
        default_angle: Option<f64>,
        literal: bool,
    ) -> Result<Self, svg::Error> {
        let trace = std::env::var_os("PP_TRACE").is_some();
        let t0 = std::time::Instant::now();
        let mut outlines = Vec::new();
        let mut tiles_raw: Vec<(MP, String, usize)> = Vec::new();
        let mut grout_parts: Vec<MP> = Vec::new();
        let mut reds: Vec<Subpath> = Vec::new();
        let mut ignored: Vec<(String, usize)> = Vec::new();
        let mut note = |k: String| match ignored.iter_mut().find(|(n, _)| *n == k) {
            Some(e) => e.1 += 1,
            None => ignored.push((k, 1)),
        };
        for it in items {
            let role = stroke_role(it.stroke);
            if let Some(fill) = it.fill {
                let g = area_of(&it.subpaths);
                if !g.0.is_empty() {
                    tiles_raw.push((g, color_key(fill), it.order));
                }
            }
            let Some(stroke) = it.stroke else { continue };
            match role {
                Some(Role::Outline) if it.fill.is_some() => {
                    note("black stroke on filled shape".into())
                }
                Some(Role::Outline) => {
                    for (pts, _) in &it.subpaths {
                        if pts.len() >= 3 {
                            for p in clean(polygon(pts)) {
                                if p.unsigned_area() > 1.0 {
                                    outlines.push(p);
                                }
                            }
                        }
                    }
                }
                Some(Role::Grout) => {
                    let half = it.stroke_mm.max(0.05) / 2.0;
                    for (pts, closed) in &it.subpaths {
                        grout_parts.push(grout_shape(pts, *closed, half));
                    }
                }
                Some(Role::Path) => {
                    for (pts, closed) in &it.subpaths {
                        let n = pts.len();
                        let closed = *closed
                            || (n > 2
                                && (pts[n - 1].0 - pts[0].0).hypot(pts[n - 1].1 - pts[0].1) < 1e-3);
                        reds.push((pts.clone(), closed));
                    }
                }
                None => note(match stroke {
                    Paint::Url => "url".into(),
                    p => color_key(p),
                }),
            }
        }

        if trace {
            eprintln!(
                "items: {:.2}s, {} tiles, {} grout parts",
                t0.elapsed().as_secs_f64(),
                tiles_raw.len(),
                grout_parts.len()
            );
        }
        let grout_boxes: Vec<Option<Rect<f64>>> =
            grout_parts.iter().map(|g| g.bounding_rect()).collect();
        let grout = if grout_parts.is_empty() {
            MultiPolygon::new(vec![])
        } else {
            union_of(grout_parts.iter().flat_map(|g| g.0.iter()))
        };
        if trace {
            eprintln!("grout union: {:.2}s", t0.elapsed().as_secs_f64());
        }
        // Later shapes cover earlier ones, as they do on screen; grout cuts through everything.
        let boxes: Vec<Option<Rect<f64>>> = tiles_raw
            .iter()
            .map(|(g, _, _)| g.bounding_rect())
            .collect();
        let mut tiles = Vec::new();
        for (idx, (g, key, order)) in tiles_raw.iter().enumerate() {
            let Some(bb) = boxes[idx] else { continue };
            let mut vis = g.clone();
            let above: Vec<&Polygon<f64>> = tiles_raw
                .iter()
                .enumerate()
                .filter(|(k, (_, _, o))| {
                    *o > *order && boxes[*k].is_some_and(|b| bbox_hit(&b, &bb))
                })
                .flat_map(|(_, (gg, _, _))| gg.0.iter())
                .collect();
            if !above.is_empty() {
                vis = crate::geom::difference(&vis, &union_of(above));
            }
            if !vis.0.is_empty() {
                if let Some(vb) = vis.bounding_rect() {
                    let near: Vec<&Polygon<f64>> = grout_parts
                        .iter()
                        .enumerate()
                        .filter(|(k, _)| grout_boxes[*k].is_some_and(|b| bbox_hit(&b, &vb)))
                        .flat_map(|(_, gg)| gg.0.iter())
                        .collect();
                    if !near.is_empty() {
                        vis = crate::geom::difference(&vis, &union_of(near));
                    }
                }
            }
            for p in vis {
                if p.unsigned_area() > 0.05 {
                    let bbox = p.bounding_rect().unwrap();
                    tiles.push(Tile {
                        poly: p,
                        bbox,
                        // set below, from the red lines, the colour or the default
                        direction: Direction::Angle(0.0),
                        color: key.clone(),
                        source: Source::Default,
                    });
                }
            }
        }
        if trace {
            eprintln!(
                "occlusion and grout: {:.2}s, {} tiles",
                t0.elapsed().as_secs_f64(),
                tiles.len()
            );
        }
        if tiles.is_empty() && !(literal && !reds.is_empty()) {
            return Err(svg::Error(format!(
                "the SVG has no filled shapes (tiles){}",
                if literal { " or light red lines" } else { "" }
            )));
        }

        // Red lines set the direction of the tile they lie in ... (a tile may
        // get both a closed and an open line: rings win, as in the Python engine)
        let mut own_rings = vec![false; tiles.len()];
        let mut own_angle: Vec<Option<f64>> = vec![None; tiles.len()];
        let mut red_lines = Vec::new();
        let mut red_tiles: Vec<usize> = Vec::new();
        for (pts, closed) in &reds {
            let mut c = pts.clone();
            if *closed {
                c.push(pts[0]);
            }
            let line = LineString::from(c);
            red_lines.push(line.clone());
            if tiles.is_empty() {
                continue;
            }
            let Some(lb) = line.bounding_rect() else {
                continue;
            };
            let cand: Vec<usize> = (0..tiles.len())
                .filter(|&k| bbox_hit(&tiles[k].bbox, &lb))
                .collect();
            if literal {
                for k in cand {
                    let hit = clip_length(&tiles[k].poly, &line) > 0.0
                        || geo::Intersects::intersects(&tiles[k].poly, &line);
                    if hit && !red_tiles.contains(&k) {
                        red_tiles.push(k);
                    }
                }
                continue;
            }
            let mut best: Option<usize> = None;
            let mut best_len = 0.0;
            for &k in &cand {
                let ln = clip_length(&tiles[k].poly, &line);
                if ln > best_len {
                    best = Some(k);
                    best_len = ln;
                }
            }
            if best.is_none() {
                if let Some(pt) = point_on_line(&line) {
                    best = (0..tiles.len()).find(|&k| tiles[k].poly.contains(&pt));
                }
            }
            let Some(b) = best else { continue };
            if *closed {
                own_rings[b] = true;
            } else {
                own_angle[b] = Some(direction(pts));
            }
        }
        // ... and every other tile of the same fill colour.
        let mut by_color: Vec<(String, Vec<usize>)> = Vec::new();
        for (k, t) in tiles.iter().enumerate() {
            if own_angle[k].is_some() || own_rings[k] {
                match by_color.iter_mut().find(|(c, _)| *c == t.color) {
                    Some(e) => e.1.push(k),
                    None => by_color.push((t.color.clone(), vec![k])),
                }
            }
        }
        let mut color_rule = BTreeMap::new();
        for (key, ks) in &by_color {
            let conc = ks.iter().filter(|&&k| own_rings[k]).count();
            let angs: Vec<f64> = ks.iter().filter_map(|&k| own_angle[k]).collect();
            if conc > angs.len() {
                color_rule.insert(key.clone(), Direction::Concentric);
            } else if !angs.is_empty() {
                // most common direction (to 1 degree), then the exact mean of those lines
                let mut counts: Vec<(f64, Vec<f64>)> = Vec::new();
                for &a in &angs {
                    let b = round_half_even(a).rem_euclid(180.0);
                    match counts.iter_mut().find(|(c, _)| *c == b) {
                        Some(e) => e.1.push(a),
                        None => counts.push((b, vec![a])),
                    }
                }
                let mut best = 0;
                for i in 1..counts.len() {
                    if counts[i].1.len() > counts[best].1.len() {
                        best = i;
                    }
                }
                let (b, members) = &counts[best];
                let exact: Vec<f64> = members
                    .iter()
                    .map(|a| b + ieee_remainder(a - b, 180.0))
                    .collect();
                color_rule.insert(
                    key.clone(),
                    Direction::Angle(exact.iter().sum::<f64>() / exact.len() as f64),
                );
            }
        }
        let mut defaulted = 0;
        for (k, t) in tiles.iter_mut().enumerate() {
            if own_angle[k].is_some() || own_rings[k] {
                t.direction = Direction::from_parts(own_angle[k], own_rings[k]);
                t.source = Source::OwnLine;
                continue;
            }
            match color_rule.get(&t.color) {
                Some(d) => {
                    t.direction = *d;
                    t.source = Source::Colour;
                }
                None => {
                    t.direction =
                        Direction::Angle(default_angle.unwrap_or_else(|| long_axis(&t.poly)));
                    t.source = Source::Default;
                    defaulted += 1;
                }
            }
        }
        Ok(TileDrawing {
            tiles,
            grout,
            outlines,
            reds,
            red_lines,
            red_tiles,
            color_rule,
            literal,
            default_angle,
            defaulted,
            ignored,
            size,
        })
    }

    pub fn from_drawing(
        d: &Drawing,
        default_angle: Option<f64>,
        literal: bool,
    ) -> Result<Self, svg::Error> {
        Self::from_items(&d.items, d.size, default_angle, literal)
    }

    pub fn summary(&self) -> String {
        let mut cols: Vec<&str> = self.tiles.iter().map(|t| t.color.as_str()).collect();
        cols.sort_unstable();
        cols.dedup();
        let mut s = format!(
            "{} tiles in {} colour(s), {} outline(s), {} red line(s)",
            self.tiles.len(),
            cols.len(),
            self.outlines.len(),
            self.reds.len()
        );
        if !self.grout.0.is_empty() {
            s += ", grout";
        }
        if self.literal {
            s += &format!(
                ", red lines printed as drawn through {} tile(s)",
                self.red_tiles.len()
            );
        }
        if self.defaulted > 0 {
            s += &format!(
                ", {} tile(s) without a red line use {}",
                self.defaulted,
                if self.default_angle.is_some() {
                    "the default angle"
                } else {
                    "their long axis"
                }
            );
        }
        if !self.ignored.is_empty() {
            let parts: Vec<String> = self
                .ignored
                .iter()
                .map(|(k, n)| format!("{k} x{n}"))
                .collect();
            s += &format!("; ignored strokes: {}", parts.join(", "));
        }
        s
    }
}

// --------------------------------------------------------------------------- //
// Composing: one pattern repeat copied over a layout of piece outlines
// --------------------------------------------------------------------------- //

/// How the pattern sits on the layout.
#[derive(Debug, Clone, Copy)]
pub struct Placing {
    /// Turn the pattern this much (degrees, counter-clockwise).
    pub rotate: f64,
    /// Where the pattern's bottom-left corner goes, in layout mm.
    pub origin: (f64, f64),
    pub scale: f64,
    /// Replaces every grout width (mm).
    pub grout: Option<f64>,
    /// Without `grout`: grout widths scale with the pattern (else stay as drawn).
    pub grout_scales: bool,
    /// One copy of the repeat (its page's corner at `origin`), on a background
    /// tile ([`crate::patterns::BACKGROUND`]) covering the rest (ADR 0015).
    pub once: bool,
}

/// As written into the composed drawing and read back: 4 decimals.
fn r4(v: f64) -> f64 {
    format!("{v:.4}").parse().unwrap()
}

fn layout_outlines(layout: &Drawing) -> Vec<Polygon<f64>> {
    let mut out = Vec::new();
    for it in &layout.items {
        if stroke_role(it.stroke) == Some(Role::Outline) && it.fill.is_none() {
            for (pts, _) in &it.subpaths {
                if pts.len() >= 3 {
                    out.extend(
                        clean(polygon(pts))
                            .into_iter()
                            .filter(|p| p.unsigned_area() > 1.0),
                    );
                }
            }
        }
    }
    out
}

/// `pattern` (one repeat; its page is the repeat) copied over `layout`: the
/// composed drawing's items (pattern copies, then the layout's own shapes),
/// its page size, and a one-line description.
pub fn compose(
    pattern: &Drawing,
    layout: &Drawing,
    placing: Placing,
) -> Result<(Vec<Item>, (f64, f64), String), svg::Error> {
    let e = |m: &str| Err(svg::Error(m.to_string()));
    let (cw, ch) = pattern.size;
    let (w, h) = layout.size;
    let s = if placing.scale == 0.0 {
        1.0
    } else {
        placing.scale
    };
    if cw <= 0.0 || ch <= 0.0 {
        return e("the pattern has no page size");
    }
    if s <= 0.0 {
        return e("the pattern scale must be more than 0");
    }
    if pattern
        .items
        .iter()
        .any(|it| stroke_role(it.stroke) == Some(Role::Outline) && it.fill.is_none())
    {
        return e("the pattern has black outlines; patterns hold only the tiles (outlines belong in the layout)");
    }
    let outl = layout_outlines(layout);
    if outl.is_empty() {
        return e("the layout has no black outlines (unfilled shapes with a black stroke)");
    }
    let region = union_of(outl.iter()).buffer_with_style(
        BufferStyle::new(1.0).line_join(LineJoin::Round(std::f64::consts::PI / 32.0)),
    );
    let (sa, ca) = placing.rotate.to_radians().sin_cos();
    let (ox, oy) = placing.origin;
    let fwd = |x: f64, y: f64| {
        let (x, y) = (x * s, y * s);
        (ox + x * ca - y * sa, oy + x * sa + y * ca)
    };
    let back = |x: f64, y: f64| {
        let (x, y) = (x - ox, y - oy);
        ((x * ca + y * sa) / s, (-x * sa + y * ca) / s)
    };
    // how far the pattern's shapes hang over its page
    let pts: Vec<(f64, f64)> = pattern
        .items
        .iter()
        .flat_map(|it| it.subpaths.iter().flat_map(|(p, _)| p.iter().copied()))
        .collect();
    if pts.is_empty() {
        return e("the pattern is empty");
    }
    let tb = pts
        .iter()
        .fold((f64::MAX, f64::MAX, f64::MIN, f64::MIN), |b, &(x, y)| {
            (b.0.min(x), b.1.min(y), b.2.max(x), b.3.max(y))
        });
    let rb = region.bounding_rect().unwrap();
    let q: Vec<(f64, f64)> = [
        (rb.min().x, rb.min().y),
        (rb.min().x, rb.max().y),
        (rb.max().x, rb.min().y),
        (rb.max().x, rb.max().y),
    ]
    .iter()
    .map(|&(x, y)| back(x, y))
    .collect();
    let qx0 = q.iter().map(|p| p.0).fold(f64::MAX, f64::min);
    let qx1 = q.iter().map(|p| p.0).fold(f64::MIN, f64::max);
    let qy0 = q.iter().map(|p| p.1).fold(f64::MAX, f64::min);
    let qy1 = q.iter().map(|p| p.1).fold(f64::MIN, f64::max);
    let i0 = ((qx0 - tb.2) / cw).floor() as i64 - 1;
    let i1 = ((qx1 - tb.0) / cw).ceil() as i64 + 1;
    let j0 = ((qy0 - tb.3) / ch).floor() as i64 - 1;
    let j1 = ((qy1 - tb.1) / ch).ceil() as i64 + 1;
    let region_boxes: Vec<(Rect<f64>, &Polygon<f64>)> = region
        .0
        .iter()
        .filter_map(|p| p.bounding_rect().map(|b| (b, p)))
        .collect();
    let mut cells = Vec::new();
    if placing.once {
        cells.push((0.0, 0.0));
    } else {
        if (i1 - i0) * (j1 - j0) > 400_000 {
            return Err(svg::Error(format!(
                "the pattern repeat is too small for this layout ({} x {} mm)",
                svg::fmt_g(cw),
                svg::fmt_g(ch)
            )));
        }
        for i in i0..=i1 {
            for j in j0..=j1 {
                let (dx, dy) = (i as f64 * cw, j as f64 * ch);
                let corners = [(tb.0, tb.1), (tb.2, tb.1), (tb.2, tb.3), (tb.0, tb.3)];
                let cell = polygon(
                    &corners
                        .iter()
                        .map(|&(x, y)| fwd(dx + x, dy + y))
                        .collect::<Vec<_>>(),
                );
                let cb = cell.bounding_rect().unwrap();
                if region_boxes
                    .iter()
                    .any(|(b, p)| bbox_hit(b, &cb) && geo::Intersects::intersects(*p, &cell))
                {
                    cells.push((dx, dy));
                }
            }
        }
    }

    // The composed drawing as the Python engine writes and reads it back:
    // coordinates and widths to 4 decimals, y measured down from the 4-decimal page top.
    let h4 = r4(h);
    let place = |x: f64, y: f64| (r4(x), h4 - r4(h - y));
    let stroke_of = |it: &Item| match stroke_role(it.stroke) {
        None => None, // strokes with no meaning are dropped
        Some(_) => it.stroke,
    };
    let fill_of = |p: Option<Paint>| match p {
        Some(Paint::Url) => Some(Paint::Rgb(0xcc, 0xcc, 0xcc)),
        p => p,
    };
    let mut items: Vec<Item> = Vec::new();
    let mut push = |it: &Item, subpaths: Vec<Subpath>, stroke_mm: f64| {
        let order = items.len();
        let stroke = stroke_of(it);
        items.push(Item {
            fill: fill_of(it.fill),
            stroke,
            stroke_mm: if stroke.is_some() { r4(stroke_mm) } else { 1.0 },
            subpaths,
            order,
            id: String::new(),
            tag: "path".into(),
        });
    };
    if placing.once {
        // the background first: the copy's shapes cover it
        let m = 2.0;
        let (x0, y0, x1, y1) = (
            rb.min().x - m,
            rb.min().y - m,
            rb.max().x + m,
            rb.max().y + m,
        );
        let c = |i: usize| u8::from_str_radix(&crate::patterns::BACKGROUND[i..i + 2], 16).unwrap();
        let bg = Item {
            fill: Some(Paint::Rgb(c(1), c(3), c(5))),
            stroke: None,
            stroke_mm: 1.0,
            subpaths: Vec::new(),
            order: 0,
            id: String::new(),
            tag: "path".into(),
        };
        let ring: Vec<(f64, f64)> = [(x0, y0), (x1, y0), (x1, y1), (x0, y1)]
            .iter()
            .map(|&(x, y)| place(x, y))
            .collect();
        push(&bg, vec![(ring, true)], 1.0);
    }
    for it in &pattern.items {
        // every copy of shape 1, then of shape 2 ...: later shapes stay on top
        let sw = if stroke_role(it.stroke) == Some(Role::Grout) {
            match placing.grout {
                Some(g) => g,
                None if !placing.grout_scales => it.stroke_mm,
                None => it.stroke_mm * s,
            }
        } else {
            it.stroke_mm * s
        };
        for &(dx, dy) in &cells {
            let subs: Vec<Subpath> = it
                .subpaths
                .iter()
                .map(|(p, c)| {
                    (
                        p.iter()
                            .map(|&(x, y)| {
                                let (a, b) = fwd(x + dx, y + dy);
                                place(a, b)
                            })
                            .collect(),
                        *c,
                    )
                })
                .collect();
            push(it, subs, sw);
        }
    }
    for it in &layout.items {
        let subs: Vec<Subpath> = it
            .subpaths
            .iter()
            .map(|(p, c)| (p.iter().map(|&(x, y)| place(x, y)).collect(), *c))
            .collect();
        push(it, subs, it.stroke_mm);
    }
    let info = format!(
        "repeat {} x {} mm{} {}, turned {} deg, origin {} {}",
        svg::fmt_g(r4(cw * s)),
        svg::fmt_g(r4(ch * s)),
        if s != 1.0 {
            format!(", scale {}", svg::fmt_g(s))
        } else {
            String::new()
        },
        if placing.once {
            "placed once".to_string()
        } else {
            format!("copied {} times", cells.len())
        },
        svg::fmt_g(placing.rotate),
        svg::fmt_g(ox),
        svg::fmt_g(oy)
    );
    Ok((items, (r4(w), h4), info))
}

/// Compose and read the result into tiles (what the engine fills).
pub fn compose_tiles(
    pattern: &Drawing,
    layout: &Drawing,
    placing: Placing,
    default_angle: Option<f64>,
    literal: bool,
) -> Result<(TileDrawing, String), svg::Error> {
    let (items, size, info) = compose(pattern, layout, placing)?;
    let d = TileDrawing::from_items(&items, size, default_angle, literal)?;
    Ok((d, info))
}

#[cfg(test)]
mod tests {
    use super::*;

    const BRICKS: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" width="20mm" height="10mm" viewBox="0 0 20 10">
      <g stroke="#0000ff" stroke-width="0.4">
        <rect fill="#d8b07a" x="0" y="0" width="10" height="5"/>
        <rect fill="#d8b07a" x="10" y="0" width="10" height="5"/>
        <rect fill="#9c6a3c" x="-5" y="5" width="10" height="5"/>
        <rect fill="#9c6a3c" x="5" y="5" width="10" height="5"/>
      </g>
      <line x1="1" y1="2.5" x2="9" y2="2.5" stroke="#f08080"/>
      <path d="M6 6 L8 8 L6 8 Z" stroke="#f08080" fill="none"/>
    </svg>"##;

    #[test]
    fn tiles_grout_and_directions() {
        let d = svg::read_svg(BRICKS, None, 0.15).unwrap();
        let t = TileDrawing::from_drawing(&d, None, false).unwrap();
        assert_eq!(t.tiles.len(), 4);
        // grout (0.4 wide) is cut out of every tile: a 10 x 5 brick keeps about 9.6 x 4.6 (less at the page edge)
        let a = t.tiles[0].poly.unsigned_area();
        assert!(a < 50.0 && a > 9.6 * 4.6 - 1.0, "area {a}");
        let light: Vec<_> = t.tiles.iter().filter(|x| x.color == "#d8b07a").collect();
        assert!(light.iter().all(|x| x.direction == Direction::Angle(0.0)));
        assert_eq!(light[0].source, Source::OwnLine);
        assert_eq!(light[1].source, Source::Colour);
        let dark: Vec<_> = t.tiles.iter().filter(|x| x.color == "#9c6a3c").collect();
        assert!(dark.iter().all(|x| x.direction == Direction::Concentric));
        assert_eq!(t.color_rule.get("#d8b07a"), Some(&Direction::Angle(0.0)));
        assert!(t.summary().starts_with("4 tiles in 2 colour(s)"));
    }

    #[test]
    fn later_shapes_cover_earlier_ones() {
        let svg = r##"<svg xmlns="http://www.w3.org/2000/svg" width="10mm" height="10mm" viewBox="0 0 10 10">
          <rect fill="#ff0000" x="0" y="0" width="10" height="10"/>
          <rect fill="#00ff00" x="0" y="0" width="5" height="10"/>
        </svg>"##;
        let t =
            TileDrawing::from_drawing(&svg::read_svg(svg, None, 0.15).unwrap(), Some(30.0), false)
                .unwrap();
        assert_eq!(t.tiles.len(), 2);
        assert!((t.tiles[0].poly.unsigned_area() - 50.0).abs() < 1e-9);
        assert_eq!(t.tiles[0].direction, Direction::Angle(30.0));
        assert_eq!(t.defaulted, 2);
    }

    #[test]
    fn composes_over_a_layout() {
        let pattern = svg::read_svg(BRICKS, None, 0.15).unwrap();
        let layout = svg::read_svg(
            r##"<svg xmlns="http://www.w3.org/2000/svg" width="50mm" height="30mm" viewBox="0 0 50 30">
              <rect x="5" y="5" width="40" height="20" fill="none" stroke="#000000"/></svg>"##,
            None,
            0.15,
        )
        .unwrap();
        let placing = Placing {
            rotate: 0.0,
            origin: (0.0, 0.0),
            scale: 1.0,
            grout: None,
            grout_scales: true,
            once: false,
        };
        let (t, info) = compose_tiles(&pattern, &layout, placing, None, false).unwrap();
        assert_eq!(t.outlines.len(), 1);
        assert!(t.tiles.len() >= 16, "{} tiles", t.tiles.len());
        assert!(info.contains("repeat 20 x 10 mm"), "{info}");
        // the bricks still read their directions after composing
        assert!(t
            .tiles
            .iter()
            .filter(|x| x.color == "#9c6a3c")
            .all(|x| x.direction == Direction::Concentric));
    }
}
