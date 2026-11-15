//! The sliced source: pieces found in sliced G-code (.gcode, .gcode.3mf,
//! .bgcode), one [`Input`] per file with its plates parsed once (ADR 0008).

use crate::container::{self, Kind};
use crate::plate::Plate;
use crate::project;
use crate::session::{base64, Error, Progress, Result};
use crate::source::Source;
use geo::Polygon;
use serde_json::{json, Value};
use std::any::Any;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

fn err<T>(m: impl Into<String>) -> Result<T> {
    Err(Error::Engine(m.into()))
}

pub fn base_name(p: &Path) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// One opened sliced file.
pub struct Input {
    /// As given to `open`.
    pub path: PathBuf,
    pub kind: Kind,
    pub plates: BTreeMap<u32, Plate>,
}

/// The open sliced files.
#[derive(Default)]
pub struct SlicedSource {
    pub inputs: Vec<Input>,
}

impl SlicedSource {
    /// Read and parse sliced files. The caller reports progress 1.0 once it
    /// has checked what was opened (the edition's limits).
    pub fn open(paths: &[String], progress: Progress) -> Result<SlicedSource> {
        let mut jobs: Vec<(PathBuf, u32)> = Vec::new();
        for p in paths {
            let path = PathBuf::from(p);
            if !path.exists() {
                return err(format!("file not found: {p}"));
            }
            let numbers = container::plates(&path)
                .map_err(|e| Error::Engine(format!("can't read {p}: {e:?}")))?;
            if numbers.is_empty() {
                return err(format!("{p} has no sliced plates"));
            }
            jobs.extend(numbers.into_iter().map(|n| (path.clone(), n)));
        }
        let mut inputs: Vec<Input> = Vec::new();
        let total = jobs.len();
        for (k, (path, n)) in jobs.into_iter().enumerate() {
            progress(
                k as f64 / total as f64,
                &format!("reading {} plate {n}", base_name(&path)),
            )?;
            let kind = container::kind(&path);
            let read = |e| Error::Engine(format!("can't read {}: {e:?}", path.display()));
            let data = container::read_plate(&path, n).map_err(read)?;
            let lines: Vec<String> = container::split_lines(&data)
                .iter()
                .map(|l| String::from_utf8_lossy(l).into_owned())
                .collect();
            let cfg = if kind == Kind::Bgcode {
                Some(container::config_lines(&path, n).map_err(read)?)
            } else {
                None
            };
            let plate = Plate::new(n, lines, cfg);
            if plate.already_processed() {
                return err(format!(
                    "{} plate {n} was already processed by TessaFab; open the original",
                    base_name(&path)
                ));
            }
            plate.toolpaths(); // parse now, not on first draw
            match inputs.iter_mut().find(|i| i.path == path) {
                Some(i) => {
                    i.plates.insert(n, plate);
                }
                None => inputs.push(Input {
                    path,
                    kind,
                    plates: BTreeMap::from([(n, plate)]),
                }),
            }
        }
        Ok(SlicedSource { inputs })
    }

    pub fn plate(&self, input: usize, number: u32) -> Result<&Plate> {
        self.inputs
            .get(input)
            .and_then(|i| i.plates.get(&number))
            .ok_or_else(|| Error::Engine(format!("no plate {number} in input {input}")))
    }
}

impl Source for SlicedSource {
    fn kind(&self) -> &'static str {
        project::SLICED
    }

    fn input_paths(&self) -> Vec<PathBuf> {
        self.inputs.iter().map(|i| i.path.clone()).collect()
    }

    fn describe(&self) -> Value {
        json!({"inputs": self.inputs.iter().enumerate().map(|(k, i)| json!({
            "index": k,
            "path": i.path.to_string_lossy(),
            "container": i.kind.name(),
            "plates": i.plates.values().map(|p| p.describe(k)).collect::<Vec<_>>(),
        })).collect::<Vec<_>>()})
    }

    /// Every plate's printed pieces, by (input, plate).
    fn pieces_by_plate(&self) -> BTreeMap<(i64, i64), Vec<Polygon<f64>>> {
        let mut out = BTreeMap::new();
        for (k, i) in self.inputs.iter().enumerate() {
            for (n, plate) in &i.plates {
                out.insert((k as i64, *n as i64), plate.pieces().to_vec());
            }
        }
        out
    }

    fn surfaces_by_plate(&self) -> BTreeMap<(usize, u32), Vec<(String, f64)>> {
        let mut out: BTreeMap<(usize, u32), Vec<(String, f64)>> = BTreeMap::new();
        for (k, i) in self.inputs.iter().enumerate() {
            for (n, plate) in &i.plates {
                let list = out.entry((k, *n)).or_default();
                for s in plate.surfaces(k) {
                    list.push((
                        s["id"].as_str().unwrap_or("").to_string(),
                        s["z"].as_f64().unwrap_or(0.0),
                    ));
                }
            }
        }
        out
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// The slicer's picture of a plate (PNG bytes from a .gcode.3mf), or None.
pub fn thumbnail(path: &Path, plate: u32) -> Option<Vec<u8>> {
    use std::io::Read;
    if container::kind(path) != Kind::ThreeMf || !path.exists() {
        return None;
    }
    let mut z = zip::ZipArchive::new(std::fs::File::open(path).ok()?).ok()?;
    for name in [
        format!("Metadata/plate_{plate}.png"),
        format!("Metadata/plate_{plate}_small.png"),
    ] {
        if let Ok(mut f) = z.by_name(&name) {
            let mut data = Vec::new();
            f.read_to_end(&mut data).ok()?;
            return Some(data);
        }
    }
    None
}

/// `{"thumbnail": data URL or null}`.
pub fn thumbnail_json(path: &Path, plate: u32) -> Value {
    json!({"thumbnail": thumbnail(path, plate).map(|png| format!("data:image/png;base64,{}", base64(&png)))})
}
