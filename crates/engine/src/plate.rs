//! One plate of a sliced file, read once and described for the app: its
//! top-surface blocks grouped per layer, the area each covers, the printed
//! pieces, the top surfaces the user can switch, and the slicer's settings.
//! A port of ParsedPlate / group_blocks (postprocess.py), piece_outlines
//! (tilepattern.py) and Plate (session.py).

use crate::colour;
use crate::engine::raw_of;
use crate::gcode::{find_blocks, find_bottom, Parsed};
use crate::geom::{self, MP};
use crate::slicer::{self, SlicerConfig};
use crate::toolpaths::{self, Toolpaths};
use geo::algorithm::buffer::{BufferStyle, LineCap, LineJoin};
use geo::{
    Area, BoundingRect, Buffer, Centroid, LineString, MultiLineString, MultiPolygon, Polygon,
};
use serde_json::{json, Value};
use std::cell::OnceCell;

/// Stable id of a top surface: input, plate, layer and height ("0:1:15@3.000").
pub fn surface_id(input: usize, plate: u32, layer: usize, z: f64) -> String {
    format!("{input}:{plate}:{layer}@{z:.3}")
}

fn round_to(x: f64, n: usize) -> f64 {
    format!("{x:.n$}").parse().unwrap_or(x)
}

fn round_half_even(x: f64) -> f64 {
    if (x - x.trunc()).abs() == 0.5 {
        2.0 * (x / 2.0).round()
    } else {
        x.round()
    }
}

/// Coordinates of a (multi)polygon as GeoJSON (shapely's mapping), lightly simplified.
pub fn poly_json(g: &MP, tolerance: f64) -> Value {
    let g = geom::simplify(g, tolerance);
    let ring = |r: &LineString<f64>| Value::Array(r.0.iter().map(|c| json!([c.x, c.y])).collect());
    let poly = |p: &Polygon<f64>| {
        let mut rings = vec![ring(p.exterior())];
        rings.extend(p.interiors().iter().map(ring));
        Value::Array(rings)
    };
    if g.0.len() == 1 {
        json!({"type": "Polygon", "coordinates": poly(&g.0[0])})
    } else {
        json!({"type": "MultiPolygon", "coordinates": g.0.iter().map(poly).collect::<Vec<_>>()})
    }
}

pub struct Plate {
    pub number: u32,
    pub parsed: Parsed,
    config_lines: Option<Vec<String>>,
    config: OnceCell<SlicerConfig>,
    toolpaths: OnceCell<Toolpaths>,
    pieces: OnceCell<Vec<Polygon<f64>>>,
    raws: OnceCell<Vec<MP>>,
    solid: OnceCell<Vec<(MP, f64, f64)>>,
    surfaces: OnceCell<Vec<Value>>,
    /// The first layer's bottom surface, parsed on first use (M20).
    bottom: OnceCell<Parsed>,
}

/// A bottom surface's id: its layer's surface id, marked as the bottom.
pub fn bottom_surface_id(input: usize, plate: u32, layer: usize, z: f64) -> String {
    format!("{}/bottom", surface_id(input, plate, layer, z))
}

impl Plate {
    /// `config_lines`: the settings as `; key = value` lines when they aren't in the G-code (binary G-code).
    pub fn new(number: u32, lines: Vec<String>, config_lines: Option<Vec<String>>) -> Self {
        Plate {
            number,
            parsed: find_blocks(lines, 0.42),
            config_lines,
            config: OnceCell::new(),
            toolpaths: OnceCell::new(),
            pieces: OnceCell::new(),
            raws: OnceCell::new(),
            solid: OnceCell::new(),
            surfaces: OnceCell::new(),
            bottom: OnceCell::new(),
        }
    }

    pub fn already_processed(&self) -> bool {
        self.parsed
            .lines
            .iter()
            .take(500)
            .any(|l| crate::edition::is_engine_note(l))
    }

    /// The slicer's settings for this plate.
    pub fn config(&self) -> &SlicerConfig {
        self.config.get_or_init(|| {
            slicer::read_header_config(self.config_lines.as_ref().unwrap_or(&self.parsed.lines))
        })
    }

    pub fn toolpaths(&self) -> &Toolpaths {
        self.toolpaths
            .get_or_init(|| toolpaths::extract(&self.parsed.lines, 0.5))
    }

    /// The plate's prints as flat slabs (worked out once).
    pub fn solid(&self) -> &[(MP, f64, f64)] {
        self.solid
            .get_or_init(|| toolpaths::solid_slabs(self.toolpaths(), 0.5, 0.05))
    }

    /// The area each top-surface block's lines cover (index = block).
    pub fn block_raw(&self) -> &[MP] {
        self.raws
            .get_or_init(|| self.parsed.blocks.iter().map(raw_of).collect())
    }

    /// Area covered by top surfaces printed on later layers (None if nothing).
    pub fn cover_above(&self, layer: usize) -> Option<MP> {
        let raws = self.block_raw();
        let above: Vec<&Polygon<f64>> = self
            .parsed
            .blocks
            .iter()
            .zip(raws)
            .filter(|(b, r)| b.layer > layer && !r.0.is_empty())
            .flat_map(|(_, r)| r.0.iter())
            .collect();
        (!above.is_empty()).then(|| geom::union_rings(above))
    }

    /// Usable top-surface blocks grouped per layer (same layer marker and Z), as
    /// block indices, and `(layer, why)` for blocks that can't be rewritten.
    pub fn group_blocks(&self, no_merge: bool) -> (Vec<Vec<usize>>, Vec<(usize, String)>) {
        let ironing: Vec<usize> = self
            .parsed
            .features
            .iter()
            .filter(|(_, n)| n.contains("ironing"))
            .map(|(i, _)| *i)
            .collect();
        let mut groups: Vec<Vec<usize>> = Vec::new();
        let mut keys: Vec<(usize, i64)> = Vec::new();
        let mut skipped = Vec::new();
        for (k, b) in self.parsed.blocks.iter().enumerate() {
            if let Some(why) = &b.abort {
                skipped.push((b.layer, why.clone()));
                continue;
            }
            let key = (b.layer, (round_to(b.start.2, 3) * 1000.0).round() as i64);
            match keys.iter().position(|x| *x == key) {
                Some(i) => groups[i].push(k),
                None => {
                    keys.push(key);
                    groups.push(vec![k]);
                }
            }
        }
        let mut out = Vec::new();
        for g in groups {
            let blocks = &self.parsed.blocks;
            let (lo, hi) = (
                blocks[g[0]].first.unwrap_or(0),
                blocks[*g.last().unwrap()].last,
            );
            if no_merge || ironing.iter().any(|&i| lo <= i && i <= hi) {
                out.extend(g.into_iter().map(|k| vec![k])); // ironing follows each block: keep order
            } else {
                out.push(g);
            }
        }
        (out, skipped)
    }

    /// The printed pieces: the true outline of every part, from its outer walls
    /// on all layers, numbered top to bottom, then left to right.
    pub fn pieces(&self) -> &[Polygon<f64>] {
        self.pieces
            .get_or_init(|| piece_outlines(&self.parsed, 20.0))
    }

    /// Top surfaces of this plate, one per layer and height (worked out once).
    pub fn surfaces(&self, input: usize) -> &[Value] {
        self.surfaces
            .get_or_init(|| self.find_surfaces(input, 20.0))
    }

    fn find_surfaces(&self, input: usize, min_area: f64) -> Vec<Value> {
        let (groups, _) = self.group_blocks(false);
        let raws = self.block_raw();
        let blocks = &self.parsed.blocks;
        let key_of = |k: usize| (blocks[k].layer, round_to(blocks[k].start.2, 3));
        let mut by_key: Vec<((usize, f64), Vec<usize>)> = Vec::new();
        let mut add =
            |key: (usize, f64), ks: &[usize]| match by_key.iter_mut().find(|(k, _)| *k == key) {
                Some(e) => e.1.extend_from_slice(ks),
                None => by_key.push((key, ks.to_vec())),
            };
        for g in &groups {
            add(key_of(g[0]), g);
        }
        for (k, b) in blocks.iter().enumerate() {
            if b.abort.is_some() {
                add(key_of(k), &[]);
            }
        }
        let mut out: Vec<Value> = Vec::new();
        for ((layer, z), ks) in by_key {
            let area = (!ks.is_empty())
                .then(|| geom::union_rings(ks.iter().flat_map(|&k| raws[k].0.iter())));
            let mut reason: Option<String> = None;
            let mut hidden = false;
            match &area {
                None => {
                    reason = Some(
                        blocks
                            .iter()
                            .enumerate()
                            .find(|(k, b)| b.abort.is_some() && key_of(*k) == (layer, z))
                            .and_then(|(_, b)| b.abort.clone())
                            .unwrap_or_else(|| "unusable".into()),
                    )
                }
                Some(a) => {
                    let visible = match self.cover_above(layer) {
                        Some(c) => geom::difference(a, &c),
                        None => a.clone(),
                    };
                    hidden = geom::area(&visible) < min_area;
                }
            }
            out.push(json!({
                "id": surface_id(input, self.number, layer, z),
                "layer": layer,
                "z": z,
                "area": area.as_ref().map(|a| round_to(geom::area(a), 2)).unwrap_or(0.0),
                "hidden": hidden,
                "unusable": reason,
                "shape": area.as_ref().filter(|a| !a.0.is_empty()).map(|a| poly_json(a, 0.05)).unwrap_or(Value::Null),
            }));
        }
        out.sort_by(|a, b| {
            let ka = (a["z"].as_f64().unwrap(), a["layer"].as_u64().unwrap());
            let kb = (b["z"].as_f64().unwrap(), b["layer"].as_u64().unwrap());
            ka.partial_cmp(&kb).unwrap()
        });
        out
    }

    /// The first layer's bottom surface blocks (see [`find_bottom`]).
    pub fn bottom(&self) -> &Parsed {
        self.bottom
            .get_or_init(|| find_bottom(self.parsed.lines.clone(), 0.42))
    }

    /// The face the plate's pieces lie on, for the app: one entry per layer
    /// holding bottom surface (the first only), shaped like a top surface.
    pub fn bottom_surfaces(&self, input: usize) -> Vec<Value> {
        let blocks: Vec<&crate::gcode::Block> = self
            .bottom()
            .blocks
            .iter()
            .filter(|b| b.abort.is_none())
            .collect();
        let Some(first) = blocks.first() else {
            return Vec::new();
        };
        let (layer, z) = (first.layer, round_to(first.start.2, 3));
        let polys: Vec<_> = blocks.iter().flat_map(|b| raw_of(b).0).collect();
        let area = geom::union_rings(polys.iter());
        vec![json!({
            "id": bottom_surface_id(input, self.number, layer, z),
            "layer": layer,
            "z": z,
            "area": round_to(geom::area(&area), 2),
            "hidden": false,
            "unusable": Value::Null,
            "shape": poly_json(&area, 0.05),
        })]
    }

    /// The printable bed outline `[[x, y], ...]` from the slicer's settings (None if unknown).
    pub fn bed(&self) -> Option<Vec<[f64; 2]>> {
        let c = self.config();
        let raw = c
            .raw
            .get("printable_area")
            .or_else(|| c.raw.get("bed_shape"))
            .filter(|r| !r.is_empty())?;
        let mut pts = Vec::new();
        for p in raw.split(',') {
            let (x, y) = p.trim().split_once('x')?;
            pts.push([x.trim().parse().ok()?, y.trim().parse().ok()?]);
        }
        (pts.len() >= 3).then_some(pts)
    }

    pub fn filaments(&self) -> Vec<Value> {
        let base = colour::initial_filament(&self.parsed.lines) as u64;
        colour::filaments(self.config())
            .into_iter()
            .map(|mut f| {
                let i = f["index"].as_u64().unwrap();
                f["number"] = json!(i + 1);
                f["base"] = json!(i == base);
                f
            })
            .collect()
    }

    pub fn colour_problems(&self) -> Value {
        json!({
            "ams": colour::input_problems(self.config(), &self.parsed.lines, colour::AMS),
            "manual": colour::input_problems(self.config(), &self.parsed.lines, colour::MANUAL),
        })
    }

    /// Unset project settings' values from the slicer (first extruder).
    pub fn slicer_defaults(&self) -> Value {
        let keys: [(&str, &[&str]); 4] = [
            ("retract", &["retraction_length", "retract_length"]),
            ("retractSpeed", &["retraction_speed", "retract_speed"]),
            ("zhop", &["z_hop", "retract_lift"]),
            ("travelSpeed", &["travel_speed"]),
        ];
        let c = self.config();
        let mut out = serde_json::Map::new();
        for (key, names) in keys {
            for name in names {
                let Some(v) = c.get(name) else { continue };
                use slicer::Value as V;
                let x = match v {
                    V::Float(x) | V::Percent(x) => Some(*x),
                    V::Int(i) => Some(*i as f64),
                    V::Floats(a) | V::Percents(a) => a.first().copied().flatten(),
                    V::Ints(a) => a.first().copied().flatten().map(|i| i as f64),
                    _ => None,
                };
                if let Some(x) = x {
                    out.insert(key.into(), json!(x));
                    break;
                }
            }
        }
        Value::Object(out)
    }

    pub fn describe(&self, input: usize) -> Value {
        let t = self.toolpaths();
        let pieces: Vec<Value> = self
            .pieces()
            .iter()
            .enumerate()
            .map(|(k, p)| {
                let b = p.bounding_rect().unwrap();
                let c = p.centroid().unwrap();
                json!({"index": k + 1, "area": round_to(p.unsigned_area(), 2),
                    "bounds": [round_to(b.min().x, 3), round_to(b.min().y, 3), round_to(b.max().x, 3), round_to(b.max().y, 3)],
                    "centroid": [round_to(c.x(), 3), round_to(c.y(), 3)],
                    "outline": poly_json(&MultiPolygon::new(vec![p.clone()]), 0.02)})
            })
            .collect();
        json!({
            "number": self.number,
            "layers": t.layer_count(),
            "layerZ": t.layer_z,
            "features": t.features,
            "pieces": pieces,
            "surfaces": self.surfaces(input),
            "bottomSurfaces": self.bottom_surfaces(input),
            "slicerSettings": self.slicer_defaults(),
            "filaments": self.filaments(),
            "bed": self.bed(),
            "colourProblems": self.colour_problems(),
        })
    }
}

/// The union of every layer's outlines. Most layers repeat the one below (a
/// part's walls rise straight), and one union of hundreds of almost coincident
/// outlines is very slow, so repeats are dropped (same box within 0.01 mm and
/// area within 0.01%) and the rest merged pairwise.
fn union_layers(shapes: Vec<Polygon<f64>>) -> MP {
    let mut kept: Vec<(geo::Rect<f64>, f64, Polygon<f64>)> = Vec::new();
    for q in shapes {
        let (Some(b), a) = (q.bounding_rect(), q.unsigned_area()) else {
            continue;
        };
        let repeat = kept.iter().any(|(kb, ka, _)| {
            (kb.min().x - b.min().x).abs() < 0.01
                && (kb.min().y - b.min().y).abs() < 0.01
                && (kb.max().x - b.max().x).abs() < 0.01
                && (kb.max().y - b.max().y).abs() < 0.01
                && (ka - a).abs() <= 1e-4 * a
        });
        if !repeat {
            kept.push((b, a, q));
        }
    }
    let mut level: Vec<MP> = kept
        .into_iter()
        .map(|(_, _, q)| MultiPolygon::new(vec![q]))
        .collect();
    while level.len() > 1 {
        level = level
            .chunks(2)
            .map(|c| {
                if c.len() == 2 {
                    geom::union_rings(c[0].0.iter().chain(c[1].0.iter()))
                } else {
                    c[0].clone()
                }
            })
            .collect();
    }
    level.pop().unwrap_or_else(geom::empty)
}

/// Each layer's outer walls stroked to their width (shapely: each segment
/// buffered by half its width, resolution 2) and merged, one per layer.
fn wall_layers(p: &Parsed) -> Vec<MP> {
    let step = std::f64::consts::PI / 4.0;
    let mut out = Vec::new();
    let mut layers: Vec<&usize> = p.others.keys().collect();
    layers.sort();
    for layer in layers {
        let segs = &p.others[layer];
        let ws: Vec<_> = segs
            .iter()
            .filter(|o| o.outer && (o.x1 - o.x0).hypot(o.y1 - o.y0) > 1e-6)
            .collect();
        if ws.is_empty() {
            continue;
        }
        // one stroke per width
        let mut u: Vec<Polygon<f64>> = Vec::new();
        let mut widths: Vec<f64> = ws.iter().map(|o| o.w).collect();
        widths.sort_by(|a, b| a.partial_cmp(b).unwrap());
        widths.dedup();
        for w in widths {
            let lines: Vec<LineString<f64>> = ws
                .iter()
                .filter(|o| o.w == w)
                .map(|o| LineString::from(vec![(o.x0, o.y0), (o.x1, o.y1)]))
                .collect();
            let style = BufferStyle::new(w / 2.0)
                .line_cap(LineCap::Round(step))
                .line_join(LineJoin::Round(step));
            u.extend(MultiLineString::new(lines).buffer_with_style(style).0);
        }
        out.push(geom::union_rings(u.iter()));
    }
    out
}

/// True (non-convex) outline of every printed part, from its outer walls on all layers.
pub fn piece_outlines(p: &Parsed, min_area: f64) -> Vec<Polygon<f64>> {
    let prof = std::env::var_os("PP_PROFILE").is_some();
    let t0 = std::time::Instant::now();
    let shapes: Vec<Polygon<f64>> = wall_layers(p)
        .iter()
        .flat_map(|g| g.0.iter().map(geom::exterior_only))
        .collect();
    if shapes.is_empty() {
        return vec![];
    }
    if prof {
        let nv: usize = shapes.iter().map(|q| q.exterior().0.len()).sum();
        eprintln!(
            "pieces: layers {:.2}s, {} shapes, {} vertices",
            t0.elapsed().as_secs_f64(),
            shapes.len(),
            nv
        );
    }
    let merged = union_layers(shapes);
    if prof {
        eprintln!("pieces: union {:.2}s", t0.elapsed().as_secs_f64());
    }
    let mut out: Vec<Polygon<f64>> = merged
        .0
        .iter()
        .map(geom::exterior_only)
        .filter(|q| q.unsigned_area() >= min_area)
        .map(|q| {
            geom::simplify(&MultiPolygon::new(vec![q]), 0.02)
                .0
                .into_iter()
                .next()
                .unwrap()
        })
        .collect();
    out.sort_by(|a, b| {
        let (ca, cb) = (a.centroid().unwrap(), b.centroid().unwrap());
        let ka = (-round_half_even(ca.y() / 5.0), ca.x());
        let kb = (-round_half_even(cb.y() / 5.0), cb.x());
        ka.partial_cmp(&kb).unwrap()
    });
    out
}
