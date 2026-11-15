//! Extrusion toolpaths of a plate as packed arrays, for drawing in 3D, and the
//! printed shape as flat slabs (the grey model). A port of toolpaths.py.
//!
//! The app never receives G-code text: it gets one binary buffer per request
//! holding line segments, their feature, layer, filament and flow.

use crate::gcode::{arc_points, feature_of, is_layer_change, parse_line, Machine};
use crate::geom::{self, MP};
use geo::{Area, LineString, MultiPolygon, Polygon};
use serde_json::{json, Value};

pub const MAGIC: &[u8; 4] = b"HBTP";
/// 2: adds the filament of each segment; 3: filament per mm.
pub const FORMAT_VERSION: u32 = 3;

/// Every extrusion move of a plate.
#[derive(Debug, Clone, Default)]
pub struct Toolpaths {
    /// x0 y0 z0 x1 y1 z1 per segment.
    pub seg: Vec<[f32; 6]>,
    /// Index into `features`.
    pub feature: Vec<u8>,
    /// Layer number (0 = before the first layer change).
    pub layer: Vec<u32>,
    pub features: Vec<String>,
    pub layer_z: Vec<f64>,
    /// Filament (0-based) printing the segment.
    pub filament: Vec<u8>,
    /// Filament (mm) pushed per mm of path, for the bead's width.
    pub flow: Vec<f32>,
}

fn round4(v: f64) -> f64 {
    format!("{v:.4}").parse().unwrap_or(v)
}

/// Toolpaths of every extrusion move (travel moves are left out).
pub fn extract<S: AsRef<str>>(lines: &[S], arc_step: f64) -> Toolpaths {
    let mut m = Machine::new();
    let mut t = Toolpaths {
        layer_z: vec![0.0],
        ..Default::default()
    };
    let mut cur: u8 = 255;
    let mut layer: u32 = 0;
    let mut tool: u8 = 0;
    for line in lines {
        let line = line.as_ref();
        let s = line.trim_start();
        if s.starts_with(';') {
            let s = s.trim();
            if let Some(name) = feature_of(s) {
                let k = match t.features.iter().position(|f| *f == name) {
                    Some(k) => k,
                    None => {
                        t.features.push(name);
                        t.features.len() - 1
                    }
                };
                cur = k as u8;
            } else if is_layer_change(s) {
                layer += 1;
                t.layer_z.push(m.z);
            }
            continue;
        }
        let Some((cmd, p)) = parse_line(line) else {
            continue;
        };
        if let Some(n) = cmd.strip_prefix('T') {
            // "T<n>" on its own (a comment may follow)
            let code = s.trim().split(';').next().unwrap_or("").trim();
            if let Some(num) = code
                .strip_prefix('T')
                .filter(|r| !r.is_empty() && r.bytes().all(|c| c.is_ascii_digit()))
            {
                if let Ok(k) = num.parse::<u32>() {
                    if k < 255 {
                        tool = k as u8;
                    }
                }
            }
            let _ = n;
            continue;
        }
        let Some(mv) = m.apply(&cmd, &p) else {
            continue;
        };
        if !mv.xy || mv.de <= 1e-7 {
            continue;
        }
        let lz = &mut t.layer_z[layer as usize];
        if *lz < mv.z1 {
            *lz = mv.z1;
        }
        let (z0, z1) = (mv.z0, mv.z1);
        let pts = if (mv.cmd == "G2" || mv.cmd == "G3") && !mv.multi_turn() {
            arc_points(
                mv.x0,
                mv.y0,
                mv.x1,
                mv.y1,
                mv.i,
                mv.j,
                mv.cmd == "G2",
                arc_step,
            )
        } else {
            vec![(mv.x1, mv.y1)]
        };
        let k = pts.len() as f64;
        let mut path = 0.0;
        let (mut qx0, mut qy0) = (mv.x0, mv.y0);
        for &(qx, qy) in &pts {
            path += ((qx - qx0).powi(2) + (qy - qy0).powi(2)).sqrt();
            qx0 = qx;
            qy0 = qy;
        }
        let per_mm = if path > 1e-9 { mv.de / path } else { 0.0 };
        let (mut px, mut py) = (mv.x0, mv.y0);
        for (j, &(qx, qy)) in pts.iter().enumerate() {
            let za = z0 + (z1 - z0) * j as f64 / k;
            let zb = z0 + (z1 - z0) * (j + 1) as f64 / k;
            t.seg.push([
                px as f32, py as f32, za as f32, qx as f32, qy as f32, zb as f32,
            ]);
            t.feature.push(cur);
            t.layer.push(layer);
            t.filament.push(tool);
            t.flow.push(per_mm as f32);
            px = qx;
            py = qy;
        }
    }
    // segments printed before any feature comment get an "unknown" feature
    if t.feature.contains(&255) {
        t.features.push("unknown".into());
        let u = (t.features.len() - 1) as u8;
        for f in t.feature.iter_mut().filter(|f| **f == 255) {
            *f = u;
        }
    }
    t.layer_z = t.layer_z.iter().map(|z| round4(*z)).collect();
    t
}

impl Toolpaths {
    pub fn layer_count(&self) -> usize {
        self.layer_z.len()
    }

    /// The binary buffer for layers `from..=to`: b"HBTP", u32 version, u32 header
    /// length H, H bytes of JSON header (count, features, layerZ, layerFrom,
    /// layerTo), then f32[N*6] segments, u8[N] features, u32[N] layers,
    /// u8[N] filaments, f32[N] flow, all little-endian.
    pub fn buffer(&self, from: Option<u32>, to: Option<u32>) -> (Vec<u8>, Value) {
        let lo = from.unwrap_or(0);
        let hi = to.unwrap_or(self.layer_count().saturating_sub(1) as u32);
        let idx: Vec<usize> = (0..self.seg.len())
            .filter(|&i| self.layer[i] >= lo && self.layer[i] <= hi)
            .collect();
        let info = json!({"count": idx.len(), "features": self.features, "layerZ": self.layer_z,
            "layerFrom": from, "layerTo": to});
        let head = info.to_string().into_bytes();
        let mut out = Vec::with_capacity(12 + head.len() + idx.len() * 34);
        out.extend_from_slice(MAGIC);
        out.extend_from_slice(&FORMAT_VERSION.to_le_bytes());
        out.extend_from_slice(&(head.len() as u32).to_le_bytes());
        out.extend_from_slice(&head);
        for &i in &idx {
            for v in self.seg[i] {
                out.extend_from_slice(&v.to_le_bytes());
            }
        }
        out.extend(idx.iter().map(|&i| self.feature[i]));
        for &i in &idx {
            out.extend_from_slice(&self.layer[i].to_le_bytes());
        }
        out.extend(idx.iter().map(|&i| self.filament[i]));
        for &i in &idx {
            out.extend_from_slice(&self.flow[i].to_le_bytes());
        }
        (out, info)
    }
}

fn is_outer(feature: &str) -> bool {
    let f = feature.to_lowercase();
    f.contains("outer wall") || f.contains("external perimeter") || f.contains("overhang")
}

/// The printed shape as a stack of flat slabs `(shape, z_bottom, z_top)`, bed mm.
///
/// Each layer's shape comes from its outer-wall loops alone (filled, with
/// nested loops as holes), so it is cheap and has no infill detail. Layers
/// with (nearly) the same shape are merged into one slab.
pub fn solid_slabs(t: &Toolpaths, width: f64, tolerance: f64) -> Vec<(MP, f64, f64)> {
    use geo::algorithm::buffer::{BufferStyle, LineCap, LineJoin};
    use geo::Buffer;
    let outer: Vec<bool> = t.features.iter().map(|f| is_outer(f)).collect();
    let mut by_layer: Vec<Vec<usize>> = vec![Vec::new(); t.layer_count()];
    for i in 0..t.seg.len() {
        if outer.get(t.feature[i] as usize).copied().unwrap_or(false) {
            by_layer[t.layer[i] as usize].push(i);
        }
    }
    let mut slabs: Vec<(MP, f64, f64)> = Vec::new();
    for idx in by_layer {
        if idx.is_empty() {
            continue;
        }
        let lines: Vec<LineString<f64>> = idx
            .iter()
            .map(|&i| {
                let s = t.seg[i];
                LineString::from(vec![(s[0] as f64, s[1] as f64), (s[3] as f64, s[4] as f64)])
            })
            .collect();
        // shapely: buffer(width / 2, quad_segs=1) of the loops
        let style = BufferStyle::new(width / 2.0)
            .line_cap(LineCap::Round(std::f64::consts::PI / 2.0))
            .line_join(LineJoin::Round(std::f64::consts::PI / 2.0));
        let band = geo::MultiLineString::new(lines).buffer_with_style(style);
        // each band's outline filled; nested ones become holes (even-odd)
        let mut shape: Option<MP> = None;
        for part in band.iter() {
            let filled = MultiPolygon::new(vec![Polygon::new(part.exterior().clone(), vec![])]);
            shape = Some(match shape {
                None => filled,
                Some(s) => geom::xor(&s, &filled),
            });
        }
        let Some(shape) = shape.filter(|s| !s.0.is_empty()) else {
            continue;
        };
        let shape = geom::simplify(&shape, tolerance);
        let top = idx.iter().map(|&i| t.seg[i][5]).fold(f32::MIN, f32::max) as f64;
        if let Some(prev) = slabs.last_mut() {
            let (pa, sa) = (prev.0.unsigned_area(), shape.unsigned_area());
            if (pa - sa).abs() < 0.01 * sa && geom::area(&geom::xor(&prev.0, &shape)) < 0.02 * sa {
                prev.2 = top;
                continue;
            }
        }
        let bottom = slabs.last().map(|s| s.2).unwrap_or(0.0);
        slabs.push((shape, bottom, top));
    }
    slabs
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_moves_by_feature_and_layer() {
        let g = [
            "G90\n",
            "M82\n",
            "G1 X0 Y0 Z0.2 F600\n",
            "; FEATURE: Outer wall\n",
            "G1 X10 Y0 E1\n",
            "; CHANGE_LAYER\n",
            "G1 Z0.4\n",
            "T1\n",
            "; FEATURE: Top surface\n",
            "G1 X10 Y10 E2\n",
            "G2 X0 Y10 I-5 J0 E3\n",
        ];
        let t = extract(&g, 0.5);
        assert_eq!(t.features, ["outer wall", "top surface"]);
        assert_eq!(t.layer_z, vec![0.2, 0.4]);
        assert_eq!(t.seg[0], [0.0, 0.0, 0.2, 10.0, 0.0, 0.2]);
        assert_eq!(t.layer[1], 1);
        assert_eq!(t.filament[1], 1);
        assert!((t.flow[0] - 0.1).abs() < 1e-6);
        assert!(t.seg.len() > 3, "the arc is split into pieces");
        let (buf, info) = t.buffer(Some(1), None);
        assert_eq!(&buf[..4], MAGIC);
        assert_eq!(info["count"].as_u64().unwrap() as usize, t.seg.len() - 1);
    }
}
