//! The assembly: where each printed piece sits once the pieces are put
//! together (layout SVG import is Pro).
//!
//! A placement moves one piece from the bed into the assembly: it is turned by
//! `rotation` degrees (counter-clockwise) about the centroid of its outline on
//! the bed, and that centroid is then put at `position` (assembly mm). Pieces
//! are never mirrored.
//!
//! Each placement keeps the piece's outline as it was on the bed, so the piece
//! can be found again after re-slicing even if it moved on the bed
//! ("re-linking").

use crate::drawing::{self, Placing};
use crate::fill::{Literal, PatternedAssembly, Tile};
use crate::geom::{self, MP};
use crate::patterns::{self, Library};
use crate::svg;
use geo::{
    AffineOps, AffineTransform, BoundingRect, Centroid, LineString, Polygon, Rect, Translate,
};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};

/// Share of area two outlines may differ by and still be the same piece.
pub const MAX_MISFIT: f64 = 0.12;
/// A re-sliced piece this close to where it was hasn't moved.
pub const SAME_PLACE_MM: f64 = 0.5;
/// Page margin around the outlines in a written layout SVG.
pub const MARGIN_MM: f64 = 5.0;

#[derive(Debug, Clone, PartialEq)]
pub struct Error(pub String);

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;

fn err<T>(m: impl Into<String>) -> Result<T> {
    Err(Error(m.into()))
}

/// Affine `[a, b, d, e, xoff, yoff]` (shapely's order) taking bed mm to assembly mm.
pub type Affine = [f64; 6];

/// (input, plate) of a project.
pub type PlateKey = (i64, i64);

pub fn transform(centroid: (f64, f64), rotation: f64, position: (f64, f64)) -> Affine {
    let (s, c) = rotation.to_radians().sin_cos();
    let (cx, cy) = centroid;
    [
        c,
        -s,
        s,
        c,
        position.0 - (c * cx - s * cy),
        position.1 - (s * cx + c * cy),
    ]
}

pub fn shifted(m: Affine, dx: f64, dy: f64) -> Affine {
    [m[0], m[1], m[2], m[3], m[4] + dx, m[5] + dy]
}

fn geo_affine(m: &Affine) -> AffineTransform<f64> {
    AffineTransform::new(m[0], m[1], m[4], m[2], m[3], m[5])
}

fn round4(v: f64) -> f64 {
    (v * 1e4).round() / 1e4
}

/// A piece outline as a placement stores it: `[[x, y], ...]`, 4 decimals, not closed.
pub fn outline_coords(p: &Polygon<f64>) -> Value {
    let pts = &p.exterior().0;
    let n = pts.len().saturating_sub(1);
    json!(pts[..n]
        .iter()
        .map(|c| [round4(c.x), round4(c.y)])
        .collect::<Vec<_>>())
}

/// The polygon of a stored outline (made valid if it crosses itself).
pub fn outline_poly(coords: &Value) -> MP {
    use geo::Validation;
    let pts: Vec<(f64, f64)> = coords
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|q| Some((q.get(0)?.as_f64()?, q.get(1)?.as_f64()?)))
        .collect();
    let p = Polygon::new(LineString::from(pts), vec![]);
    if p.is_valid() {
        geom::poly_mp(&p)
    } else {
        geom::union_all(&[p])
    }
}

fn centroid(g: &MP) -> (f64, f64) {
    g.centroid().map(|c| (c.x(), c.y())).unwrap_or((0.0, 0.0))
}

fn pair(v: &Value) -> (f64, f64) {
    (v[0].as_f64().unwrap_or(0.0), v[1].as_f64().unwrap_or(0.0))
}

/// A placement (project format) for a piece; by default it stays where it is.
pub fn placement_for(
    input: i64,
    plate: i64,
    piece: usize,
    poly: &Polygon<f64>,
    rotation: f64,
    position: Option<(f64, f64)>,
) -> Value {
    let c = poly
        .centroid()
        .map(|c| (c.x(), c.y()))
        .unwrap_or((0.0, 0.0));
    let pos = match position {
        None => json!([round4(c.0), round4(c.1)]),
        Some((x, y)) => json!([x, y]),
    };
    json!({"input": input, "plate": plate, "piece": piece, "outline": outline_coords(poly),
           "rotation": rotation, "position": pos})
}

/// The piece's outline in the assembly.
pub fn placed_outline(pl: &Value) -> MP {
    let poly = outline_poly(&pl["outline"]);
    let m = transform(
        centroid(&poly),
        pl["rotation"].as_f64().unwrap_or(0.0),
        pair(&pl["position"]),
    );
    poly.affine_transform(&geo_affine(&m))
}

pub fn key_of(pl: &Value) -> PlateKey {
    (
        pl["input"].as_i64().unwrap_or(0),
        pl["plate"].as_i64().unwrap_or(0),
    )
}

// --------------------------------------------------------------------------- //
// Re-linking: which current piece each placement belongs to
// --------------------------------------------------------------------------- //

fn misfit(p: &MP, o: &MP, o_area: f64, t: (f64, f64)) -> f64 {
    geom::area(&geom::xor(&p.translate(t.0, t.1), o)) / o_area
}

/// Slide piece `p` onto outline `o` (no rotation): `((dx, dy), misfit)`.
pub fn fit_translation(p: &MP, o: &MP) -> ((f64, f64), f64) {
    let (pc, oc) = (centroid(p), centroid(o));
    let o_area = geom::area(o);
    let mut t = (oc.0 - pc.0, oc.1 - pc.1);
    let mut best = misfit(p, o, o_area, t);
    let mut step = 0.5;
    while step > 0.02 {
        let mut moved = false;
        for (dx, dy) in [(step, 0.0), (-step, 0.0), (0.0, step), (0.0, -step)] {
            let c = (t.0 + dx, t.1 + dy);
            let m = misfit(p, o, o_area, c);
            if m < best - 1e-6 {
                best = m;
                t = c;
                moved = true;
            }
        }
        if !moved {
            step /= 2.0;
        }
    }
    (t, best)
}

/// Minimum-cost assignment of rows to columns (rows <= columns): the column of each row.
pub fn hungarian(cost: &[Vec<f64>]) -> Vec<Option<usize>> {
    let n = cost.len();
    let m = cost.first().map_or(0, Vec::len);
    let inf = f64::INFINITY;
    let (mut u, mut v) = (vec![0.0; n + 1], vec![0.0; m + 1]);
    let (mut p, mut way) = (vec![0usize; m + 1], vec![0usize; m + 1]);
    for i in 1..=n {
        p[0] = i;
        let mut j0 = 0;
        let mut minv = vec![inf; m + 1];
        let mut used = vec![false; m + 1];
        loop {
            used[j0] = true;
            let (i0, mut delta, mut j1) = (p[j0], inf, 0);
            for j in 1..=m {
                if !used[j] {
                    let cur = cost[i0 - 1][j - 1] - u[i0] - v[j];
                    if cur < minv[j] {
                        minv[j] = cur;
                        way[j] = j0;
                    }
                    if minv[j] < delta {
                        delta = minv[j];
                        j1 = j;
                    }
                }
            }
            for j in 0..=m {
                if used[j] {
                    u[p[j]] += delta;
                    v[j] -= delta;
                } else {
                    minv[j] -= delta;
                }
            }
            j0 = j1;
            if p[j0] == 0 {
                break;
            }
        }
        loop {
            let j1 = way[j0];
            p[j0] = p[j1];
            j0 = j1;
            if j0 == 0 {
                break;
            }
        }
    }
    let mut res = vec![None; n];
    for j in 1..=m {
        if p[j] != 0 {
            res[p[j] - 1] = Some(j - 1);
        }
    }
    res
}

/// What became of a placement's piece after re-slicing.
#[derive(Debug, Clone, PartialEq)]
pub struct Link {
    /// "ok" (same piece, same place), "moved" (found elsewhere on the bed),
    /// "ambiguous" (identical pieces all moved, so which is which is unknown)
    /// or "missing".
    pub status: &'static str,
    /// The current piece (1-based).
    pub piece: Option<usize>,
    pub candidates: Vec<usize>,
}

impl Link {
    pub fn to_json(&self) -> Value {
        json!({"status": self.status, "piece": self.piece, "candidates": self.candidates})
    }

    pub fn placed(&self) -> bool {
        matches!(self.status, "ok" | "moved")
    }
}

/// Match placements to the pieces now on the plates (pieces numbered from 1
/// in list order). Returns the link of each placement and the pieces no
/// placement claimed, as (input, plate, piece).
pub fn relink(
    placements: &[Value],
    plates: &BTreeMap<PlateKey, Vec<Polygon<f64>>>,
) -> (Vec<Link>, Vec<(i64, i64, usize)>) {
    let mut links: Vec<Option<Link>> = vec![None; placements.len()];
    let mut claimed: BTreeSet<(PlateKey, usize)> = BTreeSet::new();
    let mut by_plate: Vec<(PlateKey, Vec<usize>)> = Vec::new();
    for (i, pl) in placements.iter().enumerate() {
        let k = key_of(pl);
        match by_plate.iter_mut().find(|(key, _)| *key == k) {
            Some((_, v)) => v.push(i),
            None => by_plate.push((k, vec![i])),
        }
    }
    let none = Vec::new();
    for (key, idxs) in &by_plate {
        let pieces: Vec<MP> = plates
            .get(key)
            .unwrap_or(&none)
            .iter()
            .map(geom::poly_mp)
            .collect();
        let refs: Vec<MP> = idxs
            .iter()
            .map(|&i| outline_poly(&placements[i]["outline"]))
            .collect();
        let mut fits: BTreeMap<(usize, usize), (f64, f64)> = BTreeMap::new();
        for (a, r) in refs.iter().enumerate() {
            let ra = geom::area(r);
            for (b, piece) in pieces.iter().enumerate() {
                if (geom::area(piece) / ra - 1.0).abs() > 0.2 {
                    continue;
                }
                let (t, mis) = fit_translation(r, piece);
                if mis <= MAX_MISFIT {
                    fits.insert((a, b), (t.0.hypot(t.1), mis));
                }
            }
        }
        // cheapest assignment: shape first, then how far the piece moved
        let cost: Vec<Vec<f64>> = (0..refs.len())
            .map(|a| {
                (0..pieces.len())
                    .map(|b| fits.get(&(a, b)).map_or(1e9, |f| f.1 * 1000.0 + f.0))
                    .collect()
            })
            .collect();
        let mut assigned: BTreeMap<usize, usize> = BTreeMap::new();
        if !refs.is_empty() && !pieces.is_empty() {
            if refs.len() <= pieces.len() {
                for (a, b) in hungarian(&cost).into_iter().enumerate() {
                    if let Some(b) = b.filter(|&b| fits.contains_key(&(a, b))) {
                        assigned.insert(a, b);
                    }
                }
            } else {
                let t: Vec<Vec<f64>> = (0..pieces.len())
                    .map(|b| cost.iter().map(|r| r[b]).collect())
                    .collect();
                for (b, a) in hungarian(&t).into_iter().enumerate() {
                    if let Some(a) = a.filter(|&a| fits.contains_key(&(a, b))) {
                        assigned.insert(a, b);
                    }
                }
            }
        }
        for (a, &i) in idxs.iter().enumerate() {
            let cands: Vec<usize> = (0..pieces.len())
                .filter(|&b| fits.contains_key(&(a, b)))
                .map(|b| b + 1)
                .collect();
            let Some(&b) = assigned.get(&a) else {
                links[i] = Some(Link {
                    status: "missing",
                    piece: None,
                    candidates: cands,
                });
                continue;
            };
            claimed.insert((*key, b));
            let (dist, mis) = fits[&(a, b)];
            let status = if dist <= SAME_PLACE_MM {
                "ok"
            } else {
                let twins = (0..pieces.len())
                    .any(|b2| b2 != b && fits.get(&(a, b2)).is_some_and(|f| f.1 <= mis + 0.02));
                if twins {
                    "ambiguous"
                } else {
                    "moved"
                }
            };
            links[i] = Some(Link {
                status,
                piece: Some(b + 1),
                candidates: cands,
            });
        }
    }
    let unplaced = plates
        .iter()
        .flat_map(|(k, pieces)| (0..pieces.len()).map(move |b| (*k, b)))
        .filter(|kb| !claimed.contains(kb))
        .map(|(k, b)| (k.0, k.1, b + 1))
        .collect();
    (links.into_iter().map(Option::unwrap).collect(), unplaced)
}

/// `{current piece index (0-based): affine bed -> page}` for one plate.
///
/// Only placements linked "ok" or "moved" count. A moved piece is first slid
/// back to where its placement saw it (pieces only slide on the bed).
pub fn plate_transforms(
    placements: &[Value],
    links: &[Link],
    pieces: &[Polygon<f64>],
    key: PlateKey,
    shift: (f64, f64),
) -> BTreeMap<usize, Affine> {
    let mut out = BTreeMap::new();
    for (pl, link) in placements.iter().zip(links) {
        if key_of(pl) != key || !link.placed() {
            continue;
        }
        let Some(piece) = link.piece else { continue };
        let r = outline_poly(&pl["outline"]);
        let (mut cx, mut cy) = centroid(&r);
        if link.status == "moved" {
            if let Some(p) = pieces.get(piece - 1) {
                let ((dx, dy), _) = fit_translation(&r, &geom::poly_mp(p));
                cx += dx;
                cy += dy;
            }
        }
        let m = transform(
            (cx, cy),
            pl["rotation"].as_f64().unwrap_or(0.0),
            pair(&pl["position"]),
        );
        out.insert(piece - 1, shifted(m, shift.0, shift.1));
    }
    out
}

// --------------------------------------------------------------------------- //
// Layout SVG (the assembly as a drawing)
// --------------------------------------------------------------------------- //

fn exteriors(g: &MP) -> impl Iterator<Item = &LineString<f64>> {
    g.0.iter().map(|p| p.exterior())
}

/// Offset added to assembly mm so every outline sits on the page with a
/// margin, and the page size.
pub fn page_shift(outlines: &[MP]) -> ((f64, f64), (f64, f64)) {
    let (mut x0, mut y0, mut x1, mut y1) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
    for c in outlines.iter().flat_map(exteriors).flat_map(|r| r.0.iter()) {
        x0 = x0.min(c.x);
        y0 = y0.min(c.y);
        x1 = x1.max(c.x);
        y1 = y1.max(c.y);
    }
    (
        (MARGIN_MM - x0, MARGIN_MM - y0),
        (x1 - x0 + 2.0 * MARGIN_MM, y1 - y0 + 2.0 * MARGIN_MM),
    )
}

/// Layout SVG text for outlines in assembly mm, and the shift: the page puts
/// assembly point (x, y) at (x + shift.0, y + shift.1) mm from its bottom-left corner.
pub fn layout_svg(
    outlines: &[MP],
    rotation: f64,
    origin: (f64, f64),
    labels: Option<&[String]>,
) -> Result<(String, (f64, f64))> {
    if outlines.iter().all(|o| o.0.is_empty()) {
        return err("the assembly has no pieces");
    }
    let ((sx, sy), (w, h)) = page_shift(outlines);
    let (ox, oy) = (origin.0 + sx, origin.1 + sy);
    let mut out = vec![
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>".to_string(),
        format!(
            "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{w:.4}mm\" height=\"{h:.4}mm\" viewBox=\"0 0 {w:.4} {h:.4}\" \
             data-pattern-rotate=\"{}\" data-pattern-origin=\"{} {}\">",
            svg::fmt_g(rotation),
            svg::fmt_g(ox),
            svg::fmt_g(oy)
        ),
    ];
    for (k, o) in outlines.iter().enumerate() {
        for r in exteriors(o) {
            let n = r.0.len().saturating_sub(1);
            let pts: Vec<String> = r.0[..n]
                .iter()
                .map(|c| format!("{:.4},{:.4}", c.x + sx, h - (c.y + sy)))
                .collect();
            let label = labels
                .map(|l| format!(" id=\"{}\"", l[k]))
                .unwrap_or_default();
            out.push(format!(
                "<path{label} fill=\"none\" stroke=\"#000000\" stroke-width=\"0.3\" d=\"M{} Z\"/>",
                pts.join(" L")
            ));
        }
    }
    out.push("</svg>\n".into());
    Ok((out.join("\n"), (sx, sy)))
}

/// The layout SVG of a project's placements (pieces labelled
/// `piece-INPUT-PLATE-PIECE`) and its shift.
pub fn placements_layout(
    placements: &[Value],
    rotation: f64,
    origin: (f64, f64),
) -> Result<(String, (f64, f64))> {
    let outlines: Vec<MP> = placements.iter().map(placed_outline).collect();
    let labels: Vec<String> = placements
        .iter()
        .map(|p| format!("piece-{}-{}-{}", p["input"], p["plate"], p["piece"]))
        .collect();
    layout_svg(&outlines, rotation, origin, Some(&labels))
}

// --------------------------------------------------------------------------- //
// From a project to the engine's pattern placement
// --------------------------------------------------------------------------- //

/// User-facing text for placements that can't be used, or None.
pub fn problems(placements: &[Value], links: &[Link]) -> Option<String> {
    let parts: Vec<String> = placements
        .iter()
        .zip(links)
        .filter(|(_, l)| matches!(l.status, "missing" | "ambiguous"))
        .map(|(pl, l)| {
            let what = format!(
                "input {} plate {} piece {}",
                pl["input"].as_i64().unwrap_or(0) + 1,
                pl["plate"],
                pl["piece"]
            );
            if l.status == "missing" {
                format!("{what} is no longer on the plate")
            } else {
                let c: Vec<String> = l.candidates.iter().map(|c| c.to_string()).collect();
                format!(
                    "{what} could be any of pieces {} (identical pieces moved on the bed)",
                    c.join(", ")
                )
            }
        })
        .collect();
    (!parts.is_empty()).then(|| parts.join("; "))
}

/// (rotation, origin, scale) of the project's pattern on the assembly.
pub fn pattern_placement(project: &Value) -> (f64, (f64, f64), f64) {
    let pp = &project["patternPlacement"];
    let rot = pp["rotation"].as_f64().unwrap_or(0.0);
    let org = if pp["origin"].is_array() {
        pair(&pp["origin"])
    } else {
        (0.0, 0.0)
    };
    let scale = pp["scale"].as_f64().unwrap_or(1.0);
    (rot, org, scale)
}

// --------------------------------------------------------------------------- //
// Pattern on each piece (ADR 0014)
// --------------------------------------------------------------------------- //

/// How the pattern is laid over the pieces (`patternPlacement.mode`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Spread {
    /// One pattern carried across the assembly (the default).
    Across,
    /// A copy of the pattern on each piece, centred on it; copies of one
    /// piece are turned alike, so they print alike.
    Each,
    /// As `Each`, with every piece's pattern turned and shifted at random
    /// (from the seed), so no two print alike.
    Vary(u64),
}

/// How the pattern is spread. Placed once (ADR 0015), a varied pattern is
/// laid as on each piece: one copy can't be shifted onto every piece.
pub fn spread(project: &Value) -> Spread {
    let pp = &project["patternPlacement"];
    match pp["mode"].as_str() {
        Some("each") => Spread::Each,
        Some("vary") if !repeats(project) => Spread::Each,
        Some("vary") => Spread::Vary(pp["seed"].as_u64().unwrap_or(1)),
        _ => Spread::Across,
    }
}

// --------------------------------------------------------------------------- //
// A pattern placed once (ADR 0015)
// --------------------------------------------------------------------------- //

/// Where a pattern placed once lines up (`patternPlacement.align`).
pub const ALIGNS: [&str; 9] = [
    "top-left",
    "top",
    "top-right",
    "left",
    "center",
    "right",
    "bottom-left",
    "bottom",
    "bottom-right",
];

/// Whether the pattern repeats (`patternPlacement.repeat`, true unless false).
pub fn repeats(project: &Value) -> bool {
    project["patternPlacement"]["repeat"] != json!(false)
}

/// The point a pattern placed once lines up at, as shares of a box's width
/// and height from its bottom-left corner.
pub fn alignment(project: &Value) -> (f64, f64) {
    let a = project["patternPlacement"]["align"]
        .as_str()
        .unwrap_or("center");
    let fx = if a.ends_with("left") {
        0.0
    } else if a.ends_with("right") {
        1.0
    } else {
        0.5
    };
    let fy = if a.starts_with("top") {
        1.0
    } else if a.starts_with("bottom") {
        0.0
    } else {
        0.5
    };
    (fx, fy)
}

/// Where the corner of a pattern placed once goes (assembly mm): its page
/// (`size`, scaled and turned about that corner) lined up with `bounds` at
/// the project's alignment, then moved by `shift`.
pub fn once_origin(
    project: &Value,
    bounds: Rect<f64>,
    size: (f64, f64),
    rot: f64,
    scale: f64,
    shift: (f64, f64),
) -> (f64, f64) {
    let (fx, fy) = alignment(project);
    let (sa, ca) = rot.to_radians().sin_cos();
    let (px, py) = (fx * size.0 * scale, fy * size.1 * scale);
    let at = (
        bounds.min().x + fx * bounds.width() + shift.0,
        bounds.min().y + fy * bounds.height() + shift.1,
    );
    (at.0 - (px * ca - py * sa), at.1 - (px * sa + py * ca))
}

/// The bounds of placements' outlines.
pub fn placements_bounds(placements: &[Value]) -> Option<Rect<f64>> {
    let mut all: Option<Rect<f64>> = None;
    for pl in placements {
        if let Some(b) = placed_outline(pl).bounding_rect() {
            all = Some(match all {
                None => b,
                Some(a) => Rect::new(
                    (a.min().x.min(b.min().x), a.min().y.min(b.min().y)),
                    (a.max().x.max(b.max().x), a.max().y.max(b.max().y)),
                ),
            });
        }
    }
    all
}

/// Share of area two outlines may differ by and still be copies.
pub const COPY_MISFIT: f64 = 0.04;
/// Rays round an outline's centroid in its signature.
const SIGNATURE: usize = 720;
/// Room left round a varied piece for its random shift, mm.
pub const VARY_SHIFT_MM: f64 = 100.0;

/// How far a ray from `c` at each of [`SIGNATURE`] angles reaches before it
/// leaves the outline for the last time.
fn signature(g: &MP, c: (f64, f64)) -> Vec<f64> {
    let segs: Vec<((f64, f64), (f64, f64))> =
        g.0.iter()
            .flat_map(|p| p.exterior().lines())
            .map(|l| {
                (
                    (l.start.x - c.0, l.start.y - c.1),
                    (l.end.x - c.0, l.end.y - c.1),
                )
            })
            .collect();
    (0..SIGNATURE)
        .map(|k| {
            let (dy, dx) = (std::f64::consts::TAU * k as f64 / SIGNATURE as f64).sin_cos();
            let mut far: f64 = 0.0;
            for &((px, py), (qx, qy)) in &segs {
                let (ex, ey) = (qx - px, qy - py);
                let den = dx * ey - dy * ex;
                if den.abs() < 1e-12 {
                    continue;
                }
                let t = (px * ey - py * ex) / den;
                let u = (px * dy - py * dx) / den;
                if t > 0.0 && (-1e-9..=1.0 + 1e-9).contains(&u) {
                    far = far.max(t);
                }
            }
            far
        })
        .collect()
}

/// Degrees in (-180, 180].
fn wrap(deg: f64) -> f64 {
    let d = deg.rem_euclid(360.0);
    if d > 180.0 {
        d - 360.0
    } else {
        d
    }
}

/// `p` turned by `deg` about its centroid and moved onto `o`'s centroid.
fn turned_onto(p: &MP, deg: f64, o: &MP) -> MP {
    p.affine_transform(&geo_affine(&transform(centroid(p), deg, centroid(o))))
}

/// The turn (degrees) that lays `p` on `o`, if `p` is a copy of `o`. When
/// several turns fit (a symmetrical piece), the smallest.
fn copy_turn(p: &MP, sp: &[f64], o: &MP, so: &[f64]) -> Option<f64> {
    let (ap, ao) = (geom::area(p), geom::area(o));
    if ao <= 0.0 || (ap - ao).abs() / ao > COPY_MISFIT {
        return None;
    }
    // the same reaches in any order, whatever the turn: a quick no for most
    // pieces that aren't copies
    let mean = so.iter().sum::<f64>() / so.len() as f64;
    let (mut a, mut b) = (sp.to_vec(), so.to_vec());
    a.sort_by(f64::total_cmp);
    b.sort_by(f64::total_cmp);
    let rms = (a.iter().zip(&b).map(|(x, y)| (x - y).powi(2)).sum::<f64>() / a.len() as f64).sqrt();
    if rms > 0.05 * mean {
        return None;
    }
    let n = SIGNATURE;
    // err[s]: turning p by s steps; p's ray j - s then lands on o's ray j
    let err: Vec<f64> = (0..n)
        .map(|s| {
            (0..n)
                .map(|j| {
                    let d = sp[(j + n - s) % n] - so[j];
                    d * d
                })
                .sum()
        })
        .collect();
    let best = err.iter().cloned().fold(f64::MAX, f64::min);
    // a turn worth checking: near the best, or close in its own right (rays
    // within 2% of the mean reach), since a symmetrical piece fits at several
    // turns and a slicer's small notches can make the wrong one the best
    let near = (best * 4.0 + 1e-6).max(n as f64 * (0.02 * mean).powi(2));
    let step = 360.0 / n as f64;
    let mut fits: Vec<(f64, f64)> = Vec::new();
    for s in 0..n {
        let (a, b, c) = (err[(s + n - 1) % n], err[s], err[(s + 1) % n]);
        if b > a || b > c || b > near {
            continue;
        }
        // the parabola's lowest point between the neighbours
        let den = a - 2.0 * b + c;
        let off = if den > 1e-12 {
            0.5 * (a - c) / den
        } else {
            0.0
        };
        let deg = wrap((s as f64 + off.clamp(-0.5, 0.5)) * step);
        let m = geom::area(&geom::xor(&turned_onto(p, deg, o), o)) / ao;
        if m <= COPY_MISFIT {
            fits.push((deg, m));
        }
    }
    let least = fits.iter().map(|f| f.1).fold(f64::MAX, f64::min);
    fits.into_iter()
        .filter(|f| f.1 <= least + 0.01)
        .map(|f| f.0)
        .min_by(|a, b| a.abs().total_cmp(&b.abs()))
}

/// Which outlines are copies of one another, at any turn: per outline, its
/// group (the index of the group's first outline) and the turn in degrees
/// about its centroid that lays it on that first outline (0 for the first).
pub fn copies(outlines: &[MP]) -> Vec<(usize, f64)> {
    let sigs: Vec<Vec<f64>> = outlines.iter().map(|o| signature(o, centroid(o))).collect();
    let mut firsts: Vec<usize> = Vec::new();
    let mut out = Vec::with_capacity(outlines.len());
    for (k, o) in outlines.iter().enumerate() {
        let found = firsts
            .iter()
            .find_map(|&f| copy_turn(o, &sigs[k], &outlines[f], &sigs[f]).map(|t| (f, t)));
        match found {
            Some(g) => out.push(g),
            None => {
                firsts.push(k);
                out.push((k, 0.0));
            }
        }
    }
    out
}

/// The copy group of each of a project's placements (see [`copies`]).
pub fn copy_groups(project: &Value) -> Vec<usize> {
    let placements = project["assembly"]["placements"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    let outlines: Vec<MP> = placements.iter().map(placed_outline).collect();
    copies(&outlines).into_iter().map(|c| c.0).collect()
}

/// A small fast generator for the varied pattern (splitmix64).
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> f64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        ((z ^ (z >> 31)) >> 11) as f64 / (1u64 << 53) as f64
    }
}

/// One piece's way into the pattern when the pattern is on each piece.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PieceFrame {
    /// The placement as the pattern sees it: rotation and position.
    pub rotation: f64,
    pub position: (f64, f64),
    /// Varied: the piece's own frame (centred, copies turned alike) into the
    /// pattern's, `[a, b, d, e, xoff, yoff]`; identity otherwise.
    pub vary: Affine,
}

/// Where the pattern sees each placement. Across: where the assembly puts
/// it. Each: turned to match the first of its copies, its centroid at
/// (0, 0), so every piece takes the same part of the pattern. Vary: as Each,
/// then turned at random and set apart on a grid at a random shift.
pub fn piece_frames(project: &Value) -> Vec<PieceFrame> {
    let placements = project["assembly"]["placements"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    let id = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];
    let mode = spread(project);
    if mode == Spread::Across {
        return placements
            .iter()
            .map(|pl| PieceFrame {
                rotation: pl["rotation"].as_f64().unwrap_or(0.0),
                position: pair(&pl["position"]),
                vary: id,
            })
            .collect();
    }
    let outlines: Vec<MP> = placements.iter().map(placed_outline).collect();
    let groups = copies(&outlines);
    let base: Vec<f64> = placements
        .iter()
        .zip(&groups)
        .map(|(pl, g)| round4(wrap(pl["rotation"].as_f64().unwrap_or(0.0) + g.1)))
        .collect();
    let Spread::Vary(seed) = mode else {
        return base
            .into_iter()
            .map(|rotation| PieceFrame {
                rotation,
                position: (0.0, 0.0),
                vary: id,
            })
            .collect();
    };
    // a grid wide enough that no two pieces, wherever their shift puts them, meet
    let reach = outlines
        .iter()
        .flat_map(|o| {
            let c = centroid(o);
            o.0.iter()
                .flat_map(|p| p.exterior().0.iter())
                .map(move |q| (q.x - c.0).hypot(q.y - c.1))
                .collect::<Vec<_>>()
        })
        .fold(0.0, f64::max);
    let pitch = 2.0 * reach + VARY_SHIFT_MM + 10.0;
    let cols = (placements.len() as f64).sqrt().ceil().max(1.0) as usize;
    base.into_iter()
        .enumerate()
        .map(|(k, rotation)| {
            let mut rng = Rng(seed ^ (k as u64).wrapping_mul(0xd1b5_4a32_d192_ed03));
            let turn = rng.next() * 360.0;
            let (sx, sy) = (rng.next() * VARY_SHIFT_MM, rng.next() * VARY_SHIFT_MM);
            let (gx, gy) = (
                round4((k % cols) as f64 * pitch + sx),
                round4((k / cols) as f64 * pitch + sy),
            );
            let (s, c) = turn.to_radians().sin_cos();
            PieceFrame {
                rotation: round4(wrap(rotation + turn)),
                position: (gx, gy),
                vary: [c, -s, s, c, gx, gy],
            }
        })
        .collect()
}

/// The project's placements as the pattern sees them (see [`piece_frames`]).
pub fn pattern_placements(project: &Value) -> Vec<Value> {
    let placements = project["assembly"]["placements"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    if spread(project) == Spread::Across {
        return placements.to_vec();
    }
    placements
        .iter()
        .zip(piece_frames(project))
        .map(|(pl, f)| {
            let mut pl = pl.clone();
            pl["rotation"] = json!(f.rotation);
            pl["position"] = json!([f.position.0, f.position.1]);
            pl
        })
        .collect()
}

fn mirrored_x(g: &MP) -> MP {
    g.affine_transform(&AffineTransform::new(-1.0, 0.0, 0.0, 0.0, 1.0, 0.0))
}

/// The patterned assembly of one plate of a project whose assembly has
/// placements. `links` come from [`relink`] for the project's placements;
/// `pieces` are the plate's current piece outlines; `grout` overrides the
/// project's (the colour layers under the top use none).
pub fn build_placement(
    project: &Value,
    links: &[Link],
    pieces: &[Polygon<f64>],
    key: PlateKey,
    lib: &Library,
    grout: Option<f64>,
    log: &mut dyn FnMut(&str),
) -> Result<PatternedAssembly> {
    let assembled = project["assembly"]["placements"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    let r = &project["pattern"];
    if r.is_null() {
        return err("choose a pattern first");
    }
    let mode = spread(project);
    let (rot, org, scale) = pattern_placement(project);
    let seen = pattern_placements(project);
    let placements: &[Value] = if mode == Spread::Across {
        assembled
    } else {
        &seen
    };
    let (layout_text, shift) = if mode == Spread::Across {
        placements_layout(placements, rot, org)?
    } else {
        // the pieces as the pattern sees them, and their mirror images about
        // x = 0, so the faces seen from below (mirrored there) find tiles too
        let outlines: Vec<MP> = placements
            .iter()
            .map(placed_outline)
            .flat_map(|o| [mirrored_x(&o), o])
            .collect();
        layout_svg(&outlines, rot, org, None)?
    };
    let directions = &project["tileDirections"];
    let repeat = patterns::resolve(
        Some(r),
        lib,
        patterns::baked_directions(Some(r), Some(directions)),
    )
    .map_err(|e| Error(e.0))?;
    let read = |t: &str| svg::read_svg(t, None, 0.15).map_err(|e| Error(e.0));
    let settings = &project["patternSettings"];
    let literal = settings["svgPaths"] == "literal";
    let once = !repeats(project);
    let pattern = read(&repeat)?;
    // placed once, the origin is a shift from where the copy lines up
    let at = if once {
        let bounds = placements_bounds(placements)
            .ok_or_else(|| Error("the assembly has no pieces".into()))?;
        once_origin(project, bounds, pattern.size, rot, scale, org)
    } else {
        org
    };
    let placing = Placing {
        rotate: rot,
        origin: (at.0 + shift.0, at.1 + shift.1),
        scale,
        grout: grout.or(project["grout"].as_f64()),
        grout_scales: false,
        once,
    };
    let (mut drawing, info) = drawing::compose_tiles(
        &pattern,
        &read(&layout_text)?,
        placing,
        settings["svgDefaultAngle"].as_f64(),
        literal,
    )
    .map_err(|e| Error(e.0))?;
    log(&format!("svg: {info}\n"));
    // the background beyond a copy placed once runs straight, with the pattern
    for t in drawing.tiles.iter_mut() {
        if t.color == patterns::BACKGROUND && t.source == drawing::Source::Default {
            t.direction = crate::direction::Direction::Angle(rot);
        }
    }
    patterns::apply_directions(&mut drawing, directions.as_object(), rot);
    if pieces.is_empty() {
        return err("no outer walls found, so the printed pieces can't be located");
    }
    let transforms = plate_transforms(placements, links, pieces, key, shift);
    let mut ts = vec![None; pieces.len()];
    for (k, m) in transforms {
        if k >= pieces.len() {
            return err(format!(
                "placement for piece {}, but the plate has {} piece(s)",
                k + 1,
                pieces.len()
            ));
        }
        ts[k] = Some(m);
    }
    let placed = ts.iter().filter(|t| t.is_some()).count();
    let tiles = drawing
        .tiles
        .iter()
        .map(|t| Tile {
            poly: t.poly.clone(),
            bbox: t.bbox,
            direction: t.direction,
            color: t.color.clone(),
        })
        .collect();
    log(&format!(
        "svg: {}\nsvg: {} piece(s), {placed} placed in the assembly\n",
        drawing.summary(),
        pieces.len()
    ));
    let mut placement =
        PatternedAssembly::new(pieces.iter().map(geom::poly_mp).collect(), ts, tiles);
    // the layout's page is the assembly moved by `shift`
    placement.assembly_frame = [1.0, 0.0, 0.0, 1.0, shift.0, shift.1];
    if mode != Spread::Across {
        placement.mirror_x = Some(shift.0);
        // where each piece sits in the assembly, for drawing it as assembled
        let mut views = vec![None; pieces.len()];
        for (k, m) in plate_transforms(assembled, links, pieces, key, (0.0, 0.0)) {
            if k < views.len() {
                views[k] = Some(m);
            }
        }
        placement.views = Some(views);
    }
    if literal {
        placement.literal = Some(Literal::new(
            drawing.red_lines.clone(),
            drawing.red_tiles.clone(),
            drawing.grout.clone(),
        ));
    }
    Ok(placement)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn square(x: f64, y: f64, w: f64, h: f64) -> Polygon<f64> {
        Polygon::new(
            LineString::from(vec![(x, y), (x + w, y), (x + w, y + h), (x, y + h), (x, y)]),
            vec![],
        )
    }

    #[test]
    fn transform_turns_about_the_centroid_then_moves_it() {
        let m = transform((10.0, 0.0), 90.0, (0.0, 5.0));
        let (x, y) = (
            m[0] * 10.0 + m[1] * 0.0 + m[4],
            m[2] * 10.0 + m[3] * 0.0 + m[5],
        );
        assert!((x - 0.0).abs() < 1e-9 && (y - 5.0).abs() < 1e-9);
    }

    fn ell() -> Polygon<f64> {
        Polygon::new(
            LineString::from(vec![
                (0.0, 0.0),
                (30.0, 0.0),
                (30.0, 10.0),
                (10.0, 10.0),
                (10.0, 25.0),
                (0.0, 25.0),
                (0.0, 0.0),
            ]),
            vec![],
        )
    }

    fn hexagon(r: f64) -> Polygon<f64> {
        let pts: Vec<(f64, f64)> = (0..=6)
            .map(|k| {
                let a = (k as f64 * 60.0).to_radians();
                (r * a.cos(), r * a.sin())
            })
            .collect();
        Polygon::new(LineString::from(pts), vec![])
    }

    fn turned(p: &Polygon<f64>, deg: f64, to: (f64, f64)) -> MP {
        let g = geom::poly_mp(p);
        g.affine_transform(&geo_affine(&transform(centroid(&g), deg, to)))
    }

    #[test]
    fn copies_are_found_at_any_turn() {
        let outlines = vec![
            turned(&ell(), 0.0, (0.0, 0.0)),
            turned(&hexagon(20.0), 0.0, (100.0, 0.0)),
            turned(&ell(), 37.0, (50.0, 80.0)),
            turned(&hexagon(20.0), 10.0, (-60.0, 5.0)),
            // a mirrored L is not a copy (pieces are never mirrored)
            geom::poly_mp(&ell())
                .affine_transform(&AffineTransform::new(-1.0, 0.0, 0.0, 0.0, 1.0, 0.0)),
            turned(&hexagon(25.0), 0.0, (0.0, 0.0)),
        ];
        let c = copies(&outlines);
        let groups: Vec<usize> = c.iter().map(|g| g.0).collect();
        assert_eq!(groups, vec![0, 1, 0, 1, 4, 5]);
        assert!((c[2].1 + 37.0).abs() < 0.3, "{c:?}");
        // a hexagon fits every 60 degrees: the smallest turn
        assert!((c[3].1 + 10.0).abs() < 0.3, "{c:?}");
        // turning each copy as found lays it on its group's first
        for (k, (g, t)) in c.iter().enumerate() {
            let m = geom::area(&geom::xor(
                &turned_onto(&outlines[k], *t, &outlines[*g]),
                &outlines[*g],
            )) / geom::area(&outlines[*g]);
            assert!(m < 0.01, "{k}: {m}");
        }
    }

    fn project(mode: &str, outlines: &[MP]) -> Value {
        let placements: Vec<Value> = outlines
            .iter()
            .enumerate()
            .map(|(k, o)| {
                json!({"input": 0, "plate": 1, "piece": k + 1, "outline": outline_coords(&o.0[0]),
                       "rotation": 0.0, "position": [k as f64 * 40.0, 0.0]})
            })
            .collect();
        json!({"assembly": {"placements": placements},
               "patternPlacement": {"origin": null, "rotation": null, "scale": 1.0, "mode": mode, "seed": 7}})
    }

    #[test]
    fn each_piece_sees_the_pattern_from_its_own_centre() {
        let outlines = vec![
            turned(&ell(), 0.0, (10.0, 10.0)),
            turned(&ell(), 120.0, (90.0, 40.0)),
        ];
        let across = project("across", &outlines);
        assert_eq!(spread(&across), Spread::Across);
        assert_eq!(
            pattern_placements(&across),
            across["assembly"]["placements"].as_array().unwrap().clone()
        );

        let each = project("each", &outlines);
        assert_eq!(spread(&each), Spread::Each);
        let seen: Vec<MP> = pattern_placements(&each)
            .iter()
            .map(placed_outline)
            .collect();
        for o in &seen {
            let c = centroid(o);
            assert!(c.0.abs() < 1e-6 && c.1.abs() < 1e-6, "{c:?}");
        }
        // the copies lie one on the other, so they take the same tiles
        let m = geom::area(&geom::xor(&seen[0], &seen[1])) / geom::area(&seen[0]);
        assert!(m < 0.01, "{m}");
        assert_eq!(copy_groups(&each), vec![0, 0]);
    }

    #[test]
    fn varied_pieces_are_turned_apart_and_never_meet() {
        let outlines: Vec<MP> = (0..5)
            .map(|k| turned(&ell(), k as f64 * 30.0, (k as f64 * 50.0, 0.0)))
            .collect();
        let vary = project("vary", &outlines);
        assert_eq!(spread(&vary), Spread::Vary(7));
        let frames = piece_frames(&vary);
        let seen: Vec<MP> = pattern_placements(&vary)
            .iter()
            .map(placed_outline)
            .collect();
        for i in 0..seen.len() {
            for j in i + 1..seen.len() {
                assert!(
                    geom::is_empty(&geom::intersection(&seen[i], &seen[j])),
                    "{i} {j}"
                );
            }
        }
        // turns differ from piece to piece, and the same seed gives the same ones
        assert!(frames
            .windows(2)
            .any(|w| (w[0].rotation - w[1].rotation).abs() > 1.0));
        assert_eq!(frames, piece_frames(&vary));
        let mut other = vary.clone();
        other["patternPlacement"]["seed"] = json!(8);
        assert_ne!(frames, piece_frames(&other));
        // each frame takes the piece's centred, turned-alike outline to where the pattern sees it
        let mut each_mode = vary.clone();
        each_mode["patternPlacement"]["mode"] = json!("each");
        let each: Vec<MP> = pattern_placements(&each_mode)
            .iter()
            .map(placed_outline)
            .collect();
        for (k, f) in frames.iter().enumerate() {
            let moved = each[k].affine_transform(&geo_affine(&f.vary));
            let m = geom::area(&geom::xor(&moved, &seen[k])) / geom::area(&seen[k]);
            assert!(m < 1e-3, "{k}: {m}");
        }
    }

    #[test]
    fn hungarian_finds_the_cheapest_assignment() {
        let c = vec![
            vec![4.0, 1.0, 3.0],
            vec![2.0, 0.0, 5.0],
            vec![3.0, 2.0, 2.0],
        ];
        assert_eq!(hungarian(&c), vec![Some(1), Some(0), Some(2)]);
    }

    #[test]
    fn relink_follows_moved_pieces() {
        let a = square(0.0, 0.0, 20.0, 10.0);
        let b = square(40.0, 0.0, 10.0, 10.0);
        let pls = vec![
            placement_for(0, 1, 1, &a, 0.0, None),
            placement_for(0, 1, 2, &b, 0.0, None),
        ];
        // re-sliced: the square moved, the rectangle stayed, their order swapped
        let now = BTreeMap::from([((0, 1), vec![square(60.0, 5.0, 10.0, 10.0), a.clone()])]);
        let (links, unplaced) = relink(&pls, &now);
        assert_eq!((links[0].status, links[0].piece), ("ok", Some(2)));
        assert_eq!((links[1].status, links[1].piece), ("moved", Some(1)));
        assert!(unplaced.is_empty());
        let t = plate_transforms(&pls, &links, &now[&(0, 1)], (0, 1), (0.0, 0.0));
        // the moved square goes back where its placement put it
        let m = t[&0];
        let (x, y) = (
            m[0] * 65.0 + m[1] * 10.0 + m[4],
            m[2] * 65.0 + m[3] * 10.0 + m[5],
        );
        assert!((x - 45.0).abs() < 0.05 && (y - 5.0).abs() < 0.05, "{x} {y}");
    }
}
