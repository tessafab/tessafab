//! The G-code target: the sliced files rewritten with the pattern on their
//! top surfaces (ADR 0008). It needs a sliced source, since it rewrites that
//! source's own files, and keeps each plate's result for the app's views
//! (toolpaths, the assembled view) until the project changes.

use crate::assembly;
use crate::checks::{self, Mismatch};
use crate::container::{self, Kind};
use crate::edition::{self, Edition};
use crate::generate;
use crate::geom::{self, MP};
use crate::library::py_float;
use crate::plate::poly_json;
use crate::project;
use crate::session::{Error, Progress, Result};
use crate::sliced::{base_name, SlicedSource};
use crate::target::{placements, Context, Target};
use crate::toolpaths::{self, Toolpaths};
use geo::{AffineOps, AffineTransform, BoundingRect};
use serde_json::{json, Value};
use std::any::Any;
use std::collections::BTreeMap;
use std::path::Path;

fn err<T>(m: impl Into<String>) -> Result<T> {
    Err(Error::Engine(m.into()))
}

fn engine<E: std::fmt::Display>(e: E) -> Error {
    Error::Engine(e.to_string())
}

/// The sliced files this target rewrites.
fn sliced<'a>(cx: &Context<'a>) -> Result<&'a SlicedSource> {
    cx.source
        .as_any()
        .downcast_ref::<SlicedSource>()
        .ok_or_else(|| Error::Engine("G-code can only be written for sliced files".into()))
}

/// One rewritten plate of the Result.
pub struct PlateResult {
    pub lines: Vec<String>,
    pub rewritten: usize,
    pub kept: usize,
    pub log: String,
    /// Tool changes and purge, for multi-colour plates.
    pub colour: Option<Value>,
    /// Layers raised tiles added after the original last one.
    pub added_layers: usize,
    toolpaths: Option<Toolpaths>,
}

/// Kept-as-sliced reasons that are the user's own choice, not worth a warning.
const CHOSEN: [&str; 2] = ["not selected", "above --max-z"];

/// Write toolpath layers to a buffer file; the description the app reads.
fn write_buffer(t: &Toolpaths, path: &Path, from: Option<u32>, to: Option<u32>) -> Result<Value> {
    let (bytes, mut info) = t.buffer(from, to);
    std::fs::write(path, bytes)
        .map_err(|e| Error::Engine(format!("can't write {}: {e}", path.display())))?;
    info["path"] = json!(path.to_string_lossy());
    Ok(info)
}

fn opt(v: Option<u32>) -> String {
    v.map_or("None".into(), |n| n.to_string())
}

/// The G-code target and its last result.
#[derive(Default)]
pub struct GcodeTarget {
    result: Option<BTreeMap<(usize, u32), PlateResult>>,
    /// Assembled buffers written so far (each gets its own file).
    assembled: usize,
}

impl GcodeTarget {
    pub fn new() -> Self {
        Self::default()
    }

    /// Rewrite every plate's top surfaces with the project's pattern (the
    /// session has checked the project is ready).
    fn generate_plates(&mut self, cx: &Context, progress: Progress) -> Result<Value> {
        let src = sliced(cx)?;
        let jobs: Vec<(usize, u32)> = src
            .inputs
            .iter()
            .enumerate()
            .flat_map(|(k, i)| i.plates.keys().map(move |n| (k, *n)))
            .collect();
        let mut result = BTreeMap::new();
        let n_inputs = src.inputs.len();
        for (j, &(k, n)) in jobs.iter().enumerate() {
            let input = &src.inputs[k];
            let plate = &input.plates[&n];
            let mut stopped: Option<Error> = None;
            let mut step = |done: usize, total: usize| {
                let f = done as f64 / total.max(1) as f64;
                let f = if total == 0 { 1.0 } else { f };
                match progress(
                    (j as f64 + f) / jobs.len() as f64,
                    &format!("plate {} of {}", j + 1, jobs.len()),
                ) {
                    Ok(()) => true,
                    Err(e) => {
                        stopped = Some(e);
                        false
                    }
                }
            };
            let r = generate::generate_plate(
                cx.project,
                cx.links,
                plate,
                k,
                n_inputs,
                &base_name(&input.path),
                cx.library,
                &mut step,
            );
            if let Some(e) = stopped {
                return Err(e);
            }
            let mut r = r.map_err(Error::Engine)?;
            if cx.edition == Edition::Trial {
                edition::mark_trial(&mut r.lines);
            }
            result.insert(
                (k, n),
                PlateResult {
                    lines: r.lines,
                    rewritten: r.rewritten,
                    kept: r.kept,
                    log: r.log,
                    colour: r.colour,
                    added_layers: r.added_layers,
                    toolpaths: None,
                },
            );
        }
        progress(1.0, "done")?;
        let plates: Vec<Value> = result
            .iter()
            .map(|((k, n), r)| {
                json!({"input": k, "plate": n, "rewritten": r.rewritten, "kept": r.kept, "log": r.log, "colour": r.colour})
            })
            .collect();
        self.result = Some(result);
        Ok(json!({"plates": plates}))
    }

    /// Printer-state problems and surface warnings of the Result.
    fn findings(&self, src: &SlicedSource) -> Result<(Vec<String>, Vec<Value>, Vec<Value>)> {
        let Some(result) = &self.result else {
            return err("there is no result yet");
        };
        let (mut problems, mut warnings, mut plates) = (Vec::new(), Vec::new(), Vec::new());
        for ((k, n), r) in result {
            let name = format!("{} plate {n}", base_name(&src.inputs[*k].path));
            let orig = &src.inputs[*k].plates[n].parsed.lines;
            for m in checks::state_mismatches_raised(orig, &r.lines, 1e-3, 5, r.added_layers) {
                problems.push(format!("{name}, {}", mismatch_text(&m)));
            }
            for (layer, reason) in checks::kept_surfaces(&r.log) {
                if !CHOSEN.iter().any(|c| reason.starts_with(c)) {
                    warnings
                        .push(json!({"input": k, "plate": n, "layer": layer, "reason": reason}));
                }
            }
            plates.push(json!({"input": k, "plate": n, "rewritten": r.rewritten, "kept": r.kept, "colour": r.colour}));
        }
        Ok((problems, warnings, plates))
    }

    /// Check the current Result without writing it.
    /// The rewritten G-code of a plate from the last generate, if any.
    pub fn result_lines(&self, input: usize, plate: u32) -> Option<&[String]> {
        self.result
            .as_ref()?
            .get(&(input, plate))
            .map(|r| r.lines.as_slice())
    }

    fn write(&self, src: &SlicedSource, k: usize, dst: &Path) -> Result<()> {
        let inp = &src.inputs[k];
        let result = self.result.as_ref().unwrap();
        let text = |n: u32| result[&(k, n)].lines.concat().into_bytes();
        let fail =
            |e: container::Error| Error::Engine(format!("can't write {}: {e:?}", dst.display()));
        if inp.kind == Kind::ThreeMf {
            let plates: BTreeMap<u32, Vec<u8>> =
                inp.plates.keys().map(|n| (*n, text(*n))).collect();
            // a Bambu project lists the filaments each plate uses: plates that now
            // use more get their lists updated
            let mut use_by_plate: BTreeMap<u32, BTreeMap<usize, f64>> = BTreeMap::new();
            for (n, plate) in &inp.plates {
                let r = &result[&(k, *n)];
                if r.colour
                    .as_ref()
                    .is_some_and(|c| c["changes"].as_u64().unwrap_or(0) > 0)
                {
                    let start = crate::colour::initial_filament(&plate.parsed.lines) as usize;
                    use_by_plate.insert(*n, crate::colour::filament_use(&r.lines, start));
                }
            }
            let mut edit = |name: &str, data: &[u8]| -> Option<Vec<u8>> {
                let text = String::from_utf8_lossy(data);
                if name == "Metadata/slice_info.config" {
                    let mut t = text.into_owned();
                    for (n, u) in &use_by_plate {
                        t = crate::colour::update_slice_info(&t, *n, u, inp.plates[n].config());
                    }
                    return Some(t.into_bytes());
                }
                if name == "Metadata/filament_sequence.json" {
                    let mut t = if text.trim().is_empty() {
                        "{}".to_string()
                    } else {
                        text.into_owned()
                    };
                    for (n, u) in &use_by_plate {
                        t = crate::colour::update_filament_sequence(&t, *n, u);
                    }
                    return Some(t.into_bytes());
                }
                let n: u32 = name
                    .strip_prefix("Metadata/plate_")?
                    .strip_suffix(".json")?
                    .parse()
                    .ok()?;
                let u = use_by_plate.get(&n)?;
                crate::colour::update_plate_json(&text, u, inp.plates[&n].config())
                    .map(String::into_bytes)
            };
            let colours = !use_by_plate.is_empty();
            let edit: Option<&mut dyn FnMut(&str, &[u8]) -> Option<Vec<u8>>> =
                if colours { Some(&mut edit) } else { None };
            container::write_3mf(&inp.path, dst, &plates, edit).map_err(fail)
        } else {
            let n = *inp.plates.keys().next().unwrap();
            container::write_plate(&inp.path, dst, n, &text(n)).map_err(fail)
        }
    }

    /// Read the written file back: plates present, checksums right.
    fn verify(&self, src: &SlicedSource, k: usize, dst: &Path) -> Result<()> {
        let inp = &src.inputs[k];
        match inp.kind {
            Kind::ThreeMf => {
                let plates = container::plates(dst).map_err(|e| {
                    Error::Engine(format!("{} doesn't read back: {e:?}", dst.display()))
                })?;
                if plates != inp.plates.keys().copied().collect::<Vec<_>>() {
                    return err(format!("{} is missing plates after writing", dst.display()));
                }
                let bad = container::md5_mismatches(dst).map_err(|e| {
                    Error::Engine(format!("{} doesn't read back: {e:?}", dst.display()))
                })?;
                if let Some(n) = bad.first() {
                    return err(format!(
                        "{}: plate {n} checksum doesn't match after writing",
                        dst.display()
                    ));
                }
            }
            Kind::Bgcode => {
                let data = std::fs::read(dst).map_err(engine)?;
                crate::bgcode::BgcodeFile::parse(&data)
                    .and_then(|f| f.gcode())
                    .map_err(|e| {
                        Error::Engine(format!("{} doesn't read back: {e:?}", dst.display()))
                    })?;
            }
            Kind::Gcode => {}
        }
        Ok(())
    }

    // ------------------------------------------------- the app's G-code views

    /// Write a plate's toolpaths (original, or the Result's if generated) to a buffer file.
    pub fn toolpaths(
        &mut self,
        cx: &Context,
        input: usize,
        plate: u32,
        from: Option<u32>,
        to: Option<u32>,
    ) -> Result<Value> {
        let src = sliced(cx)?;
        src.plate(input, plate)?;
        let path = cx.work.join(format!(
            "toolpaths-{input}-{plate}-{}-{}.bin",
            opt(from),
            opt(to)
        ));
        let tps = self.plate_toolpaths(src, input, plate);
        write_buffer(tps, &path, from, to)
    }

    fn plate_toolpaths<'a>(
        &'a mut self,
        src: &'a SlicedSource,
        input: usize,
        plate: u32,
    ) -> &'a Toolpaths {
        if let Some(r) = self
            .result
            .as_mut()
            .and_then(|r| r.get_mut(&(input, plate)))
        {
            if r.toolpaths.is_none() {
                r.toolpaths = Some(toolpaths::extract(&r.lines, 0.5));
            }
        }
        match self.result.as_ref().and_then(|r| r.get(&(input, plate))) {
            Some(r) => r.toolpaths.as_ref().unwrap(),
            None => src.inputs[input].plates[&plate].toolpaths(),
        }
    }

    /// Every placed piece's toolpaths (the Result's, once generated) moved into
    /// the assembly. Layers in the buffer are the distinct heights across all plates.
    pub fn assembled_toolpaths(&mut self, cx: &Context) -> Result<Value> {
        let src = sliced(cx)?;
        let placements = placements(cx.project);
        if placements.is_empty() {
            return err("the assembly has no placed pieces");
        }
        let mut all = Toolpaths::default();
        let keys: Vec<(usize, u32)> = src
            .inputs
            .iter()
            .enumerate()
            .flat_map(|(k, i)| i.plates.keys().map(move |n| (k, *n)))
            .collect();
        for (k, n) in keys {
            let pieces = src.inputs[k].plates[&n].pieces().to_vec();
            let transforms = assembly::plate_transforms(
                &placements,
                cx.links,
                &pieces,
                (k as i64, n as i64),
                (0.0, 0.0),
            );
            if transforms.is_empty() {
                continue;
            }
            let tps = self.plate_toolpaths(src, k, n);
            if tps.seg.is_empty() {
                continue;
            }
            let remap: Vec<u8> = tps
                .features
                .iter()
                .map(|f| match all.features.iter().position(|x| x == f) {
                    Some(i) => i as u8,
                    None => {
                        all.features.push(f.clone());
                        (all.features.len() - 1) as u8
                    }
                })
                .collect();
            for (idx, m) in &transforms {
                let region = geom::buffer(&geom::poly_mp(&pieces[*idx]), 1.0, true);
                let Some(bb) = region.bounding_rect() else {
                    continue;
                };
                for i in 0..tps.seg.len() {
                    let s = tps.seg[i];
                    let (x, y) = (s[0] as f64, s[1] as f64);
                    if x < bb.min().x
                        || x > bb.max().x
                        || y < bb.min().y
                        || y > bb.max().y
                        || !geom::contains_pt(&region, x, y)
                    {
                        continue;
                    }
                    let mv = |px: f32, py: f32| {
                        let (px, py) = (px as f64, py as f64);
                        (
                            (m[0] * px + m[1] * py + m[4]) as f32,
                            (m[2] * px + m[3] * py + m[5]) as f32,
                        )
                    };
                    let (a, b) = (mv(s[0], s[1]), mv(s[3], s[4]));
                    all.seg.push([a.0, a.1, s[2], b.0, b.1, s[5]]);
                    all.feature.push(remap[tps.feature[i] as usize]);
                    all.filament.push(tps.filament[i]);
                    all.flow.push(tps.flow[i]);
                }
            }
        }
        if all.seg.is_empty() {
            return err("none of the placed pieces could be found on their plates");
        }
        // layers: the distinct heights (to 3 decimals) across all plates
        let key = |z: f32| ((z as f64) * 1000.0).round() as i64;
        let mut zs: Vec<i64> = all.seg.iter().map(|s| key(s[5])).collect();
        zs.sort_unstable();
        zs.dedup();
        all.layer = all
            .seg
            .iter()
            .map(|s| zs.binary_search(&key(s[5])).unwrap() as u32)
            .collect();
        all.layer_z = zs.iter().map(|z| *z as f64 / 1000.0).collect();
        self.assembled += 1;
        let path = cx.work.join(format!("assembled-{}.bin", self.assembled));
        write_buffer(&all, &path, None, None)
    }

    /// Every placed piece as solid slabs moved into the assembly, for drawing:
    /// `{"slabs": [{"z0", "z1", "shape"}]}`, shapes GeoJSON-style, assembly mm.
    pub fn assembled_solid(&self, cx: &Context) -> Result<Value> {
        let src = sliced(cx)?;
        let placements = placements(cx.project);
        let mut out = Vec::new();
        for (k, inp) in src.inputs.iter().enumerate() {
            for (n, plate) in &inp.plates {
                let pieces = plate.pieces();
                let transforms = assembly::plate_transforms(
                    &placements,
                    cx.links,
                    pieces,
                    (k as i64, *n as i64),
                    (0.0, 0.0),
                );
                if transforms.is_empty() {
                    continue;
                }
                let slabs = plate.solid();
                for (idx, m) in &transforms {
                    let region = geom::buffer(&geom::poly_mp(&pieces[*idx]), 1.0, true);
                    let rb = region.bounding_rect();
                    for (shape, z0, z1) in slabs {
                        if let (Some(a), Some(b)) = (rb, shape.bounding_rect()) {
                            if !geom::bbox_hit(&a, &b) {
                                continue;
                            }
                        }
                        let part = geom::intersection(shape, &region);
                        if geom::is_empty(&part) || geom::area(&part) < 0.5 {
                            continue;
                        }
                        let t = AffineTransform::new(m[0], m[1], m[4], m[2], m[3], m[5]);
                        let part: MP = part.affine_transform(&t);
                        out.push(json!({"z0": z0, "z1": z1, "shape": poly_json(&part, 0.02)}));
                    }
                }
            }
        }
        Ok(json!({"slabs": out}))
    }
}

impl Target for GcodeTarget {
    fn kind(&self) -> &'static str {
        project::GCODE
    }

    fn info(&self) -> Value {
        json!({"kind": project::GCODE, "name": "G-code",
               "extensions": ["gcode", "gcode.3mf", "bgcode"],
               "about": "The sliced files with the pattern printed on their top surfaces, in the same format as each input."})
    }

    fn clear(&mut self) {
        self.result = None;
    }

    fn generate(&mut self, cx: &Context, progress: Progress) -> Result<Value> {
        self.generate_plates(cx, progress)
    }

    fn check(&self, cx: &Context) -> Result<Value> {
        let (problems, warnings, plates) = self.findings(sliced(cx)?)?;
        Ok(
            json!({"ok": problems.is_empty(), "problems": problems, "warnings": warnings, "plates": plates}),
        )
    }

    fn check_export(&self, cx: &Context, paths: &[String]) -> Result<()> {
        let src = sliced(cx)?;
        if paths.len() != src.inputs.len() {
            return err(format!(
                "give one output path per input ({})",
                src.inputs.len()
            ));
        }
        for (inp, dst) in src.inputs.iter().zip(paths) {
            let dst = Path::new(dst);
            if dst.exists() && same_file(dst, &inp.path) {
                return err(format!(
                    "refusing to overwrite the input {}",
                    inp.path.display()
                ));
            }
        }
        Ok(())
    }

    /// Generate a fresh Result, check it, and write it: one output path per
    /// input, in the same container as the input. Never overwrites an input.
    fn export(&mut self, cx: &Context, paths: &[String], progress: Progress) -> Result<Value> {
        let src = sliced(cx)?;
        let summary = self.generate_plates(cx, &mut |f, m| progress(0.85 * f, m))?;
        progress(0.85, "checking")?;
        let (failures, warnings, _) = self.findings(src)?;
        if !failures.is_empty() {
            return err(format!(
                "export stopped, the result doesn't return the printer to where the original does: {}",
                failures.iter().take(5).cloned().collect::<Vec<_>>().join("; ")
            ));
        }
        progress(0.9, "writing")?;
        let mut written = Vec::new();
        for (k, dst) in paths.iter().enumerate() {
            let dst = Path::new(dst);
            let done = self
                .write(src, k, dst)
                .and_then(|_| self.verify(src, k, dst));
            if let Err(e) = done {
                let _ = std::fs::remove_file(dst);
                return Err(e);
            }
            written.push(dst.to_string_lossy().into_owned());
        }
        progress(1.0, "done")?;
        Ok(json!({"written": written, "warnings": warnings, "plates": summary["plates"]}))
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

fn same_file(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(x), Ok(y)) => x == y,
        _ => false,
    }
}

/// Python's repr of a state value.
fn state_repr(v: Option<f64>) -> String {
    v.map_or("None".into(), py_float)
}

fn mismatch_text(m: &Mismatch) -> String {
    match m {
        Mismatch::Layers { want, got } => {
            format!("the end: layers is {got}, the original has {want}")
        }
        Mismatch::Axis {
            index,
            axis,
            want,
            got,
        } => format!(
            "layer change {index}: {axis} is {}, the original has {}",
            state_repr(*got),
            state_repr(*want)
        ),
    }
}
