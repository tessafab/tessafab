//! Filling the pieces of a patterned assembly with lines: each tile with
//! parallel lines at its line direction, or with concentric rings, clipped to
//! a region and kept clear of grout. A port of TileDrawing.paths and
//! Placement.paths in tilepattern.py.
//!
//! Nothing here is about any craft: a target decides the line spacing (a
//! printed line width, a stitch row, a laser hatch) and what to do with the
//! lines (ADR 0008).

use crate::direction::Direction;
use crate::geom::{self, MP};
use geo::{
    AffineOps, AffineTransform, Area, BoundingRect, Coord, LineString, Point, Polygon, Rect,
    Translate,
};
use std::collections::HashMap;

/// A line path (segments end to end), its spacing scale, and its tile group.
pub type FillPath = (Vec<((f64, f64), (f64, f64))>, f64, String);

/// One tile of the pattern, in the pattern's frame (mm).
#[derive(Clone)]
pub struct Tile {
    pub poly: Polygon<f64>,
    pub bbox: Rect<f64>,
    pub direction: Direction,
    pub color: String,
}

/// The pattern laid over the assembled pieces, independent of any craft: each
/// piece's outline, how it sits in the pattern, and the tiles.
///
/// Each piece is in its own frame: the frame the target works in, such as the
/// bed of the plate it is printed on for G-code (a laser sheet or an
/// embroidery hoop for other targets). `transforms[k]` is the affine (shapely
/// order `[a, b, d, e, xoff, yoff]`) taking piece k from its frame into the
/// pattern's frame, where the tiles are; `None` leaves the piece unpatterned.
/// [`PatternedAssembly::paths`] gives the fill lines back in the pieces' frames.
pub struct PatternedAssembly {
    /// Piece outlines, each in its own frame.
    pub pieces: Vec<MP>,
    pub piece_boxes: Vec<Rect<f64>>,
    /// Piece frame to pattern frame, per piece.
    pub transforms: Vec<Option<[f64; 6]>>,
    pub tiles: Vec<Tile>,
    inset_cache: HashMap<(usize, i64), MP>,
    /// Insets and rings per distinct tile shape (a pattern repeats the same
    /// tiles, shifted), relative to the shape's bounding-box corner; keyed by
    /// rounded area, size and spacing, then matched geometrically.
    pub shape_cache: HashMap<(i64, i64, i64, i64), Vec<CachedShape>>,
    pub shape_lookups: usize,
    /// Red lines printed as drawn (`"svgPaths": "literal"`).
    pub literal: Option<Literal>,
    /// The assembly (mm) to the pattern's frame, shapely order.
    pub assembly_frame: [f64; 6],
    /// With the pattern on each piece (ADR 0014): the line in the pattern's
    /// frame the faces seen from below are mirrored about (else the middle
    /// of the tiles).
    pub mirror_x: Option<f64>,
    /// With the pattern on each piece: each piece's frame into the assembly
    /// as the user put it together, for drawing the pieces as assembled
    /// (else `transforms`, which are both).
    pub views: Option<Vec<Option<[f64; 6]>>>,
}

/// Red lines printed as drawn: each line is a printed path (cut where it
/// crosses grout), and the tiles they run through keep their hatching one
/// line width clear of them (TileDrawing.paths, literal).
pub struct Literal {
    pub lines: Vec<LineString<f64>>,
    /// Indices of the tiles the lines run through.
    pub tiles: Vec<usize>,
    pub grout: MP,
    /// Per spacing: the grout widened by half a line, and each line's clearance.
    cache: HashMap<i64, (MP, Vec<MP>)>,
}

impl Literal {
    pub fn new(lines: Vec<LineString<f64>>, tiles: Vec<usize>, grout: MP) -> Self {
        Literal {
            lines,
            tiles,
            grout,
            cache: HashMap::new(),
        }
    }

    fn at(&mut self, s: f64) -> &(MP, Vec<MP>) {
        let key = (s * 1e4).round() as i64;
        let (grout, lines) = (&self.grout, &self.lines);
        self.cache.entry(key).or_insert_with(|| {
            let g = if geom::is_empty(grout) {
                geom::empty()
            } else {
                geom::buffer(grout, s / 2.0, false)
            };
            // shapely: line.buffer(s, cap_style=2): flat ends, round joins
            let style = geo::algorithm::buffer::BufferStyle::new(s)
                .line_cap(geo::algorithm::buffer::LineCap::Butt)
                .line_join(geo::algorithm::buffer::LineJoin::Round(
                    std::f64::consts::PI / 32.0,
                ));
            let clear = lines
                .iter()
                .map(|l| geo::Buffer::buffer_with_style(l, style.clone()))
                .collect();
            (g, clear)
        })
    }
}

pub struct CachedShape {
    rel: Polygon<f64>,
    inset: MP,
    rings: Vec<LineString<f64>>,
}

/// The same shape within `tol` mm: every vertex of each lies that close to the
/// other's outline (vertex counts and ring starts may differ between copies).
fn same_shape(a: &Polygon<f64>, b: &Polygon<f64>, tol: f64) -> bool {
    use geo::{Distance, Euclidean};
    if a.interiors().len() != b.interiors().len() {
        return false;
    }
    let near = |p: &Polygon<f64>, q: &Polygon<f64>| {
        std::iter::once(p.exterior()).chain(p.interiors()).all(|r| {
            r.0.iter().all(|c| {
                let pt = Point::new(c.x, c.y);
                std::iter::once(q.exterior())
                    .chain(q.interiors())
                    .any(|qr| Euclidean.distance(&pt, qr) <= tol)
            })
        })
    };
    near(a, b) && near(b, a)
}

fn affine(m: &[f64; 6]) -> AffineTransform<f64> {
    // shapely order [a, b, d, e, xoff, yoff]
    AffineTransform::new(m[0], m[1], m[4], m[2], m[3], m[5])
}

impl PatternedAssembly {
    /// Pieces (each in its own frame, mm), the affine taking each into the
    /// pattern (None: the piece is left unpatterned) and the pattern's tiles.
    pub fn new(pieces: Vec<MP>, transforms: Vec<Option<[f64; 6]>>, tiles: Vec<Tile>) -> Self {
        let piece_boxes = pieces
            .iter()
            .map(|p| {
                p.bounding_rect()
                    .unwrap_or(Rect::new((0.0, 0.0), (0.0, 0.0)))
            })
            .collect();
        PatternedAssembly {
            pieces,
            piece_boxes,
            transforms,
            tiles,
            inset_cache: HashMap::new(),
            shape_cache: HashMap::new(),
            shape_lookups: 0,
            literal: None,
            assembly_frame: [1.0, 0.0, 0.0, 1.0, 0.0, 0.0],
            mirror_x: None,
            views: None,
        }
    }

    /// The same assembly for the faces the pieces lie on, seen from below:
    /// each piece's way into the pattern is mirrored in x (about the middle of
    /// the tiles), so its lines come back mirrored on the bed and read on the
    /// finished face as the pattern reads from above. Tiles, directions and
    /// red lines are unchanged.
    pub fn seen_from_below(&self) -> PatternedAssembly {
        let (lo, hi) = self
            .tiles
            .iter()
            .fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), t| {
                (lo.min(t.bbox.min().x), hi.max(t.bbox.max().x))
            });
        let cx = match self.mirror_x {
            Some(x) => x,
            None if lo.is_finite() => (lo + hi) / 2.0,
            None => 0.0,
        };
        // x' = 2 cx - x after the piece's own transform [a, b, d, e, xoff, yoff]
        let transforms = self
            .transforms
            .iter()
            .map(|t| t.map(|m| [-m[0], -m[1], m[2], m[3], 2.0 * cx - m[4], m[5]]))
            .collect();
        let mut out = self.with_tiles(self.tiles.clone());
        out.transforms = transforms;
        out
    }

    /// The same pieces and placement with other tiles.
    pub fn with_tiles(&self, tiles: Vec<Tile>) -> PatternedAssembly {
        let mut out = PatternedAssembly::new(self.pieces.clone(), self.transforms.clone(), tiles);
        out.literal = self
            .literal
            .as_ref()
            .map(|l| Literal::new(l.lines.clone(), l.tiles.clone(), l.grout.clone()));
        out.assembly_frame = self.assembly_frame;
        out.mirror_x = self.mirror_x;
        out.views = self.views.clone();
        out
    }

    pub fn piece_of(&self, isl: &Polygon<f64>) -> Option<usize> {
        let ib = isl.bounding_rect()?;
        let im = geom::poly_mp(isl);
        let mut best = None;
        let mut best_a = -1.0;
        for (k, p) in self.pieces.iter().enumerate() {
            let a = if geom::bbox_hit(&ib, &self.piece_boxes[k]) {
                geom::area(&geom::intersection(p, &im))
            } else {
                0.0
            };
            if a > best_a {
                best_a = a;
                best = Some(k);
            }
        }
        let k = best?;
        if best_a <= 0.5 * isl.unsigned_area() || self.transforms[k].is_none() {
            return None;
        }
        Some(k)
    }

    fn inset(&mut self, k: usize, s: f64) -> MP {
        let key = (k, (s * 1e4).round() as i64);
        if let Some(v) = self.inset_cache.get(&key) {
            return v.clone();
        }
        let v = self.shape(k, s).0;
        self.inset_cache.insert(key, v.clone());
        v
    }

    /// The tile's inset (its line-centre area) and concentric rings, worked
    /// out once per distinct tile shape and shifted into place.
    fn shape(&mut self, k: usize, s: f64) -> (MP, Vec<LineString<f64>>) {
        let poly = &self.tiles[k].poly;
        let b = self.tiles[k].bbox;
        let (ax, ay) = (b.min().x, b.min().y);
        let rel = poly.translate(-ax, -ay);
        let key = (
            (poly.unsigned_area() * 100.0).round() as i64,
            (b.width() * 100.0).round() as i64,
            (b.height() * 100.0).round() as i64,
            (s * 1e4).round() as i64,
        );
        if std::env::var_os("PP_PROFILE").is_some() {
            self.shape_lookups += 1;
        }
        let bucket = self.shape_cache.entry(key).or_default();
        let idx = match bucket.iter().position(|c| same_shape(&c.rel, &rel, 2e-3)) {
            Some(i) => i,
            None => {
                let inset = geom::buffer(&geom::poly_mp(&rel), -s / 2.0, false);
                let rings = ring_levels(&inset, s);
                bucket.push(CachedShape { rel, inset, rings });
                bucket.len() - 1
            }
        };
        let c = &bucket[idx];
        (
            c.inset.translate(ax, ay),
            c.rings.iter().map(|r| r.translate(ax, ay)).collect(),
        )
    }

    /// Fill paths for the pattern-frame region (TileDrawing.paths, non-literal).
    fn pattern_paths(&mut self, region: &MP, s: f64, min_seg: f64, rimmed: bool) -> Vec<FillPath> {
        let mut out = Vec::new();
        if geom::is_empty(region) {
            return out;
        }
        let rb = region.bounding_rect().unwrap();
        let mut clear: Vec<MP> = Vec::new();
        if let Some(lit) = self.literal.as_mut().filter(|l| !l.lines.is_empty()) {
            let (grout, bufs) = lit.at(s).clone();
            clear = bufs;
            let area = geom::difference(region, &grout);
            for line in &lit.lines {
                let hit = line
                    .bounding_rect()
                    .is_some_and(|b| geom::bbox_hit(&rb, &b));
                if !hit {
                    continue;
                }
                for piece in geom::clip_lines(&area, std::slice::from_ref(line)) {
                    if geom::length(&piece) < min_seg {
                        continue;
                    }
                    let path = piece
                        .0
                        .windows(2)
                        .map(|w| ((w[0].x, w[0].y), (w[1].x, w[1].y)))
                        .collect();
                    out.push((path, 1.0, String::new()));
                }
            }
        }
        // Lines that only graze the edge (they'd print as a row of dashes) are
        // dropped. Not inside rims: there the pattern's area already starts a
        // spacing past the last rim, and the rule only knocked out whole lines
        // running along it (leaving a gap two spacings wide).
        let core = if rimmed {
            geom::empty()
        } else {
            geom::buffer(region, -0.1, false)
        };
        for k in 0..self.tiles.len() {
            if !geom::bbox_hit(&rb, &self.tiles[k].bbox) {
                continue;
            }
            let inset = self.inset(k, s);
            if geom::is_empty(&inset) {
                continue;
            }
            let mut inner = geom::intersection(&inset, region);
            if let Some(lit) = &self.literal {
                if lit.tiles.contains(&k) && !geom::is_empty(&inner) {
                    // hatching keeps one line width clear of the red lines
                    let ib = inner.bounding_rect().unwrap();
                    let near: Vec<&Polygon<f64>> = clear
                        .iter()
                        .filter(|b| b.bounding_rect().is_some_and(|bb| geom::bbox_hit(&ib, &bb)))
                        .flat_map(|b| b.0.iter())
                        .collect();
                    if !near.is_empty() {
                        inner = geom::difference(&inner, &geom::union_rings(near));
                    }
                }
            }
            if geom::is_empty(&inner) || geom::area(&inner) < 1e-4 {
                continue;
            }
            match self.tiles[k].direction {
                Direction::Concentric => {
                    // a tile wholly inside the region keeps every ring whole: no cutting
                    let whole = (geom::area(&inner) - geom::area(&inset)).abs()
                        < 1e-9 * geom::area(&inset).max(1.0);
                    let rings = self.shape(k, s).1;
                    let colour = &self.tiles[k].color;
                    out.extend(
                        concentric(&inner, s, min_seg, &rings, &core, whole)
                            .into_iter()
                            .map(|(p, sc)| (p, sc, colour.clone())),
                    );
                }
                Direction::Angle(angle) => {
                    let colour = &self.tiles[k].color;
                    out.extend(
                        hatch(&inner, angle, s, min_seg, &inset, &core)
                            .into_iter()
                            .map(|(p, sc)| (p, sc, colour.clone())),
                    );
                }
            }
        }
        out
    }

    /// Each tile's part of `region` (in the pieces' frames, mm), back in the
    /// pieces' frames, with its tile group: the tiles that stand on a surface.
    pub fn tiles_within(&self, region: &MP) -> Vec<(MP, String)> {
        let mut out = Vec::new();
        for isl in region.iter() {
            let Some(k) = self.piece_of(isl) else {
                continue;
            };
            let t = affine(&self.transforms[k].unwrap());
            let Some(inv) = t.inverse() else { continue };
            let r = geom::poly_mp(&isl.affine_transform(&t));
            let Some(rb) = r.bounding_rect() else {
                continue;
            };
            for tile in self
                .tiles
                .iter()
                .filter(|tile| geom::bbox_hit(&rb, &tile.bbox))
            {
                let part = geom::intersection(&geom::poly_mp(&tile.poly), &r);
                if geom::is_empty(&part) {
                    continue;
                }
                out.push((part.affine_transform(&inv), tile.color.clone()));
            }
        }
        out
    }

    /// Fill paths for `fill` (a region in the pieces' frames, mm) with lines
    /// `s` apart, in the pieces' frames, and the islands of `fill` that lie on
    /// no patterned piece (Placement.paths).
    pub fn paths(
        &mut self,
        fill: &MP,
        s: f64,
        min_seg: f64,
        rimmed: bool,
    ) -> (Vec<FillPath>, Vec<Polygon<f64>>) {
        let mut out = Vec::new();
        let mut unplaced = Vec::new();
        for isl in fill.iter() {
            let Some(k) = self.piece_of(isl) else {
                unplaced.push(isl.clone());
                continue;
            };
            let m = self.transforms[k].unwrap();
            let t = affine(&m);
            let inv = t.inverse().unwrap();
            let region = geom::poly_mp(&isl.affine_transform(&t));
            let found = self.pattern_paths(&region, s, min_seg, rimmed);
            if std::env::var("PP_PROFILE").is_ok() {
                let b = isl.bounding_rect().unwrap();
                let rb = region.bounding_rect().unwrap();
                eprintln!("  island {:.0},{:.0}..{:.0},{:.0} ({:.1} mm2) piece {} -> pattern frame {:.0},{:.0}..{:.0},{:.0}: {} paths",
                    b.min().x, b.min().y, b.max().x, b.max().y, isl.unsigned_area(), k, rb.min().x, rb.min().y, rb.max().x, rb.max().y, found.len());
            }
            for (path, sc, colour) in found {
                let back = |p: (f64, f64)| {
                    let c = inv.apply(Coord { x: p.0, y: p.1 });
                    (c.x, c.y)
                };
                out.push((
                    path.into_iter().map(|(p, q)| (back(p), back(q))).collect(),
                    sc,
                    colour,
                ));
            }
        }
        (out, unplaced)
    }
}

/// Parallel lines across `region` at `angle` degrees, `s` apart, each at
/// least `min_seg` long: a plain solid fill (raised tiles' lower layers).
pub fn lines_at(region: &MP, angle: f64, s: f64, min_seg: f64) -> Vec<((f64, f64), (f64, f64))> {
    hatch(region, angle, s, min_seg, region, &geom::empty())
        .into_iter()
        .flat_map(|(segs, _)| segs)
        .collect()
}

fn exterior_coords(g: &MP) -> Vec<(f64, f64)> {
    g.iter()
        .flat_map(|p| p.exterior().0.iter().map(|c| (c.x, c.y)))
        .collect()
}

/// Parallel lines across `region`, positions from `frame` (tilepattern.hatch).
fn hatch(
    region: &MP,
    angle: f64,
    s: f64,
    min_seg: f64,
    frame: &MP,
    core: &MP,
) -> Vec<(Vec<((f64, f64), (f64, f64))>, f64)> {
    let a = angle.to_radians();
    let (ux, uy) = (a.cos(), a.sin());
    let (vx, vy) = (-uy, ux);
    let pts = exterior_coords(frame);
    let rpts = exterior_coords(region);
    if pts.is_empty() || rpts.is_empty() {
        return vec![];
    }
    let vs: Vec<f64> = pts.iter().map(|(x, y)| x * vx + y * vy).collect();
    let us: Vec<f64> = rpts.iter().map(|(x, y)| x * ux + y * uy).collect();
    let vmin = vs.iter().cloned().fold(f64::INFINITY, f64::min);
    let vmax = vs.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
    let umin = us.iter().cloned().fold(f64::INFINITY, f64::min) - 1.0;
    let umax = us.iter().cloned().fold(f64::NEG_INFINITY, f64::max) + 1.0;
    let span = vmax - vmin;
    let n = (span / s).round() as i64;
    let (offsets, sp) = if n < 1 {
        (vec![(vmin + vmax) / 2.0], s)
    } else {
        let sp = span / n as f64;
        let mut o: Vec<f64> = (0..=n).map(|k| vmin + k as f64 * sp).collect();
        o[0] += 1e-3;
        let last = o.len() - 1;
        o[last] -= 1e-3;
        (o, sp)
    };
    let scale = (sp / s).clamp(0.85, 1.15);
    let rv: Vec<f64> = rpts.iter().map(|(x, y)| x * vx + y * vy).collect();
    let rlo = rv.iter().cloned().fold(f64::INFINITY, f64::min) - 1e-6;
    let rhi = rv.iter().cloned().fold(f64::NEG_INFINITY, f64::max) + 1e-6;
    let mut segs = Vec::new();
    for (k, &v) in offsets.iter().enumerate() {
        if v < rlo || v > rhi {
            continue;
        }
        let mut p0 = (umin * ux + v * vx, umin * uy + v * vy);
        let mut p1 = (umax * ux + v * vx, umax * uy + v * vy);
        if k % 2 == 1 {
            std::mem::swap(&mut p0, &mut p1);
        }
        let (dx, dy) = (p1.0 - p0.0, p1.1 - p0.1);
        let line = LineString::from(vec![p0, p1]);
        let mut pieces = Vec::new();
        for g in geom::clip_lines(region, &[line]) {
            if geom::length(&g) < min_seg {
                continue;
            }
            if !geom::is_empty(core) && !geo::Intersects::intersects(core, &g) {
                continue;
            }
            let c0 = g.0[0];
            let c1 = *g.0.last().unwrap();
            let (mut p, mut q) = ((c0.x, c0.y), (c1.x, c1.y));
            if (q.0 - p.0) * dx + (q.1 - p.1) * dy < 0.0 {
                std::mem::swap(&mut p, &mut q);
            }
            pieces.push((p, q));
        }
        pieces.sort_by(|a, b| {
            let ka = (a.0 .0 - p0.0) * dx + (a.0 .1 - p0.1) * dy;
            let kb = (b.0 .0 - p0.0) * dx + (b.0 .1 - p0.1) * dy;
            ka.partial_cmp(&kb).unwrap()
        });
        segs.extend(pieces);
    }
    if segs.is_empty() {
        vec![]
    } else {
        vec![(segs, scale)]
    }
}

/// Every ring of a concentric fill of `frame`: its outline, then the outline
/// shrunk by s, 2s ... until nothing is left (tilepattern.concentric's loop).
fn ring_levels(frame: &MP, s: f64) -> Vec<LineString<f64>> {
    let mut out = Vec::new();
    let mut k = 0;
    let mut g = frame.clone();
    while !geom::is_empty(&g) && k < 2000 {
        out.extend(geom::rings(&g));
        k += 1;
        g = geom::buffer(frame, -(k as f64) * s, false);
    }
    out
}

/// Concentric rings (of the whole tile: `rings`) cut to `region` (tilepattern.concentric).
fn concentric(
    region: &MP,
    _s: f64,
    min_seg: f64,
    rings: &[LineString<f64>],
    core: &MP,
    whole: bool,
) -> Vec<(Vec<((f64, f64), (f64, f64))>, f64)> {
    let region = geom::buffer(region, 0.01, false);
    let rb = region.bounding_rect();
    let mut out = Vec::new();
    for r in rings {
        if geom::length(r) < 2.0 * min_seg {
            continue;
        }
        if let (Some(a), Some(b)) = (rb, r.bounding_rect()) {
            if !geom::bbox_hit(&a, &b) {
                continue;
            }
        }
        let cut = if whole {
            vec![r.clone()]
        } else {
            let cut = geom::clip_lines(&region, std::slice::from_ref(r));
            // a ring wholly inside stays whole (shapely: region.contains(ring));
            // clipping alone split it wherever the overlay snapped a vertex,
            // which on finely drawn tiles left rings in many pieces
            let len = geom::length(r);
            if (cut.iter().map(geom::length).sum::<f64>() - len).abs() <= 1e-6 * len.max(1.0) {
                vec![r.clone()]
            } else {
                cut
            }
        };
        let pieces = if cut.len() > 1 {
            geom::linemerge(cut)
        } else {
            cut
        };
        for piece in pieces {
            if geom::length(&piece) < min_seg
                || (!geom::is_empty(core) && !geo::Intersects::intersects(core, &piece))
            {
                continue;
            }
            let c = &piece.0;
            out.push((
                c.windows(2)
                    .map(|w| ((w[0].x, w[0].y), (w[1].x, w[1].y)))
                    .collect(),
                1.0,
            ));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use geo::{coord, Rect};

    fn rect(x0: f64, y0: f64, x1: f64, y1: f64) -> Polygon<f64> {
        Rect::new(coord! {x: x0, y: y0}, coord! {x: x1, y: y1}).to_polygon()
    }

    fn tile(poly: Polygon<f64>, direction: Direction, color: &str) -> Tile {
        let bbox = poly.bounding_rect().unwrap();
        Tile {
            poly,
            bbox,
            direction,
            color: color.into(),
        }
    }

    /// Two tiles side by side, one hatched across, one in rings, filling a
    /// piece moved 100 mm off the pattern: no G-code involved.
    #[test]
    fn fills_two_tiles_in_the_pieces_frame() {
        let piece = geom::poly_mp(&rect(100.0, 0.0, 120.0, 10.0));
        // piece frame -> pattern frame: shift back by 100 mm in x
        let to_pattern = [1.0, 0.0, 0.0, 1.0, -100.0, 0.0];
        let tiles = vec![
            tile(rect(0.0, 0.0, 10.0, 10.0), Direction::Angle(0.0), "#a"),
            tile(rect(10.0, 0.0, 20.0, 10.0), Direction::Concentric, "#b"),
        ];
        let mut asm = PatternedAssembly::new(vec![piece.clone()], vec![Some(to_pattern)], tiles);
        let (paths, unplaced) = asm.paths(&piece, 0.5, 0.2, true);
        assert!(unplaced.is_empty());
        let hatched: Vec<_> = paths.iter().filter(|p| p.2 == "#a").collect();
        let rings: Vec<_> = paths.iter().filter(|p| p.2 == "#b").collect();
        // tile a: horizontal lines 0.5 mm apart across its 9.5 mm inset
        let lines: usize = hatched.iter().map(|p| p.0.len()).sum();
        assert!((18..=21).contains(&lines), "{lines} hatch lines");
        for (p, q) in hatched.iter().flat_map(|p| p.0.iter()) {
            assert!((p.1 - q.1).abs() < 1e-9, "not horizontal");
            for x in [p.0, q.0] {
                assert!((100.0..=110.0).contains(&x), "x {x} outside tile a");
            }
        }
        // tile b: closed rings, in the piece's frame (x 110..120)
        assert!(rings.len() >= 5, "{} rings", rings.len());
        for (p, _) in rings.iter().flat_map(|p| p.0.iter()) {
            assert!((110.0 - 1e-6..=120.0 + 1e-6).contains(&p.0), "x {}", p.0);
        }
    }

    /// From below, a piece's lines are its lines from above, mirrored in x on the bed.
    #[test]
    fn seen_from_below_mirrors_the_lines_on_the_bed() {
        let piece = geom::poly_mp(&rect(0.0, 0.0, 10.0, 10.0));
        let tiles = vec![
            tile(rect(0.0, 0.0, 5.0, 10.0), Direction::Angle(30.0), "#a"),
            tile(rect(5.0, 0.0, 10.0, 10.0), Direction::Angle(120.0), "#b"),
        ];
        let id = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];
        let mut top = PatternedAssembly::new(vec![piece.clone()], vec![Some(id)], tiles);
        let mut below = top.seen_from_below();
        let (a, _) = top.paths(&piece, 0.5, 0.2, true);
        let (b, _) = below.paths(&piece, 0.5, 0.2, true);
        // tile #a lies on the left from above; from below its lines are on the right, mirrored
        let side = |paths: &[FillPath], group: &str| -> (f64, f64) {
            let segs: Vec<_> = paths
                .iter()
                .filter(|p| p.2 == group)
                .flat_map(|p| p.0.iter())
                .collect();
            let x = segs.iter().map(|(p, q)| (p.0 + q.0) / 2.0).sum::<f64>() / segs.len() as f64;
            let (p, q) = segs[0];
            let ang = (q.1 - p.1).atan2(q.0 - p.0).to_degrees().rem_euclid(180.0);
            (x, ang)
        };
        let (xa, aa) = side(&a, "#a");
        let (xb, ab) = side(&b, "#a");
        assert!(xa < 5.0 && xb > 5.0, "{xa} {xb}");
        assert!((aa - 30.0).abs() < 1e-6, "{aa}");
        assert!((ab - 150.0).abs() < 1e-6, "{ab}");
    }

    #[test]
    fn a_piece_without_a_transform_is_left_unpatterned() {
        let piece = geom::poly_mp(&rect(0.0, 0.0, 10.0, 10.0));
        let tiles = vec![tile(
            rect(0.0, 0.0, 10.0, 10.0),
            Direction::Angle(45.0),
            "#a",
        )];
        let mut asm = PatternedAssembly::new(vec![piece.clone()], vec![None], tiles);
        let (paths, unplaced) = asm.paths(&piece, 0.5, 0.2, true);
        assert!(paths.is_empty());
        assert_eq!(unplaced.len(), 1);
    }
}
