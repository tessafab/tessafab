//! A long-lived engine session: a source opened once (the files parsed once)
//! and a project made into results many times. Started as a port of
//! session.py.
//!
//! The session holds what belongs to no craft: the project, the links between
//! its placements and the open pieces, the pattern library and the edition's
//! limits. Where the pieces come from is its [`Source`]; what is made from
//! them are its [`Target`]s (ADR 0008).
//!
//! The desktop app drives one Session per open project through rpc.rs. Every
//! long call takes a progress callback; returning `Err(Cancelled)` from it
//! (rpc.rs does, when the app cancels) stops the call cleanly.

use crate::assembly::{self, Link};
use crate::edition::Edition;
use crate::gcode_target::GcodeTarget;
use crate::library;
use crate::patterns::{self, Library};
use crate::project;
use crate::sliced::SlicedSource;
use crate::source::{self, Source};
use crate::target::{Context, Target};
use geo::{BoundingRect, Polygon};
use serde_json::{json, Map, Value};
use std::borrow::Cow;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Why a call stopped.
#[derive(Debug, Clone, PartialEq)]
pub enum Error {
    /// A problem the user can fix (bad input, unusable project): the message is for them.
    Engine(String),
    /// The app cancelled the call.
    Cancelled,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Engine(m) => f.write_str(m),
            Error::Cancelled => f.write_str("cancelled"),
        }
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;

fn err<T>(m: impl Into<String>) -> Result<T> {
    Err(Error::Engine(m.into()))
}

impl From<patterns::Error> for Error {
    fn from(e: patterns::Error) -> Self {
        Error::Engine(e.0)
    }
}

impl From<project::Error> for Error {
    fn from(e: project::Error) -> Self {
        Error::Engine(e.0)
    }
}

impl From<assembly::Error> for Error {
    fn from(e: assembly::Error) -> Self {
        Error::Engine(e.0)
    }
}

/// Progress: `(fraction done, message)`; `Err(Cancelled)` stops the call.
pub type Progress<'a> = &'a mut dyn FnMut(f64, &str) -> Result<()>;

/// No progress reporting.
pub fn no_progress(_: f64, _: &str) -> Result<()> {
    Ok(())
}

/// Write a file whole or not at all (through a temporary file next to it).
fn write_atomic(dst: &Path, data: &[u8]) -> std::io::Result<()> {
    let tmp = dst.with_extension("tmp");
    std::fs::write(&tmp, data)?;
    std::fs::rename(&tmp, dst)
}

fn absolute(p: &Path) -> PathBuf {
    project::normpath(&std::path::absolute(p).unwrap_or_else(|_| p.to_path_buf()))
}

/// A scratch folder removed with the session.
struct WorkDir(PathBuf);

impl WorkDir {
    fn new() -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let p = std::env::temp_dir().join(format!("tessafab-{}-{nanos}", std::process::id()));
        let _ = std::fs::create_dir_all(&p);
        WorkDir(p)
    }
}

impl Drop for WorkDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

pub struct Session {
    /// The user's patterns and presets.
    pub library: Library,
    /// The open files the pieces come from.
    source: Box<dyn Source>,
    /// What can be made: G-code always, then any composed in at build time.
    targets: Vec<Box<dyn Target>>,
    pub project: Value,
    /// Per placement: what became of its piece.
    links: Vec<Link>,
    unplaced: Vec<(i64, i64, usize)>,
    work: WorkDir,
    /// What this session may do (set by the Pro plugin from the licence).
    pub edition: Edition,
}

impl Default for Session {
    fn default() -> Self {
        Self::new(None)
    }
}

/// The gap between plates in [`Session::default_placements`], in mm.
pub const PLATE_GAP: f64 = 10.0;

impl Session {
    pub fn new(library: Option<PathBuf>) -> Self {
        Session {
            library: Library::new(library),
            source: source::empty(),
            targets: vec![Box::new(GcodeTarget::new())],
            project: project::new(),
            links: Vec::new(),
            unplaced: Vec::new(),
            work: WorkDir::new(),
            edition: Edition::Source,
        }
    }

    /// Add a target (composed in at build time: a server's main, or a test).
    /// One of the same kind is replaced.
    pub fn add_target(&mut self, target: Box<dyn Target>) {
        self.targets.retain(|t| t.kind() != target.kind());
        self.targets.push(target);
    }

    /// The targets, for the app: `[{"kind", "name", "extensions", "about"}]`.
    pub fn targets(&self) -> Value {
        json!(self.targets.iter().map(|t| t.info()).collect::<Vec<_>>())
    }

    /// A target, by kind (to read its last result).
    pub fn target(&self, kind: &str) -> Option<&dyn Target> {
        self.targets.iter().find(|t| t.kind() == kind).map(|t| &**t)
    }

    /// The open source.
    pub fn source(&self) -> &dyn Source {
        &*self.source
    }

    /// The open sliced files, if the source is sliced G-code.
    pub fn sliced(&self) -> Option<&SlicedSource> {
        self.source.as_any().downcast_ref::<SlicedSource>()
    }

    /// Whether files are open.
    pub fn is_open(&self) -> bool {
        !self.source.is_empty()
    }

    fn target_index(&self, kind: &str) -> Result<usize> {
        self.targets
            .iter()
            .position(|t| t.kind() == kind)
            .ok_or_else(|| Error::Engine(format!("this version of TessaFab can't make {kind}")))
    }

    /// Forget every target's result (the project or the files changed).
    fn clear_results(&mut self) {
        for t in &mut self.targets {
            t.clear();
        }
    }

    /// Call `f` with a target and the context it works from: the project as
    /// the edition patterns it (see [`Edition::patterned`]).
    fn with_target<T>(
        &mut self,
        kind: &str,
        f: impl FnOnce(&mut dyn Target, &Context) -> Result<T>,
    ) -> Result<T> {
        self.with_target_on(kind, false, f)
    }

    /// As [`Session::with_target`]; `whole`: every placement, patterned or
    /// not (the app's views of the assembly show them all).
    fn with_target_on<T>(
        &mut self,
        kind: &str,
        whole: bool,
        f: impl FnOnce(&mut dyn Target, &Context) -> Result<T>,
    ) -> Result<T> {
        let k = self.target_index(kind)?;
        let project = if whole {
            Cow::Borrowed(&self.project)
        } else {
            self.edition.patterned(&self.project)
        };
        let n = self.patterned_links();
        let cx = Context {
            source: &*self.source,
            project: &project,
            links: if whole { &self.links } else { &self.links[..n] },
            library: &self.library,
            edition: self.edition,
            work: &self.work.0,
        };
        f(&mut *self.targets[k], &cx)
    }

    /// How many of the links belong to placements the edition patterns.
    fn patterned_links(&self) -> usize {
        self.edition
            .patterned_count(self.placements().len())
            .min(self.links.len())
    }

    /// The G-code target, for the app's views of its result.
    fn gcode_view<T>(
        &mut self,
        f: impl FnOnce(&mut GcodeTarget, &Context) -> Result<T>,
    ) -> Result<T> {
        self.gcode_view_on(false, f)
    }

    /// As [`Session::gcode_view`], over every placement when `whole`.
    fn gcode_view_on<T>(
        &mut self,
        whole: bool,
        f: impl FnOnce(&mut GcodeTarget, &Context) -> Result<T>,
    ) -> Result<T> {
        self.with_target_on(project::GCODE, whole, |t, cx| {
            let g = t
                .as_any_mut()
                .downcast_mut::<GcodeTarget>()
                .expect("the gcode target is a GcodeTarget");
            f(g, cx)
        })
    }

    pub fn workdir(&self) -> &Path {
        &self.work.0
    }

    pub fn set_library(&mut self, dir: &Path) {
        self.library = Library::new(Some(dir.to_path_buf()));
    }

    // ------------------------------------------------------------------ inputs

    /// Read and parse files as the project's kind of source (replacing any open ones).
    pub fn open(&mut self, paths: &[String], progress: Progress) -> Result<Value> {
        let kind = self.project["source"]["kind"]
            .as_str()
            .unwrap_or(project::SLICED)
            .to_string();
        let opened = source::open(&kind, paths, progress)?;
        let by_plate = opened.pieces_by_plate();
        let pieces: usize = by_plate.values().map(Vec::len).sum();
        self.edition
            .check_open(by_plate.len(), pieces)
            .map_err(Error::Engine)?;
        progress(1.0, "done")?;
        self.source = opened;
        self.project["source"]["inputs"] = json!(self
            .source
            .input_paths()
            .iter()
            .map(|p| json!({"path": absolute(p).to_string_lossy()}))
            .collect::<Vec<_>>());
        self.clear_results();
        Ok(self.describe())
    }

    pub fn describe(&self) -> Value {
        self.source.describe()
    }

    /// Every plate's pieces, by (input, plate).
    pub fn pieces_by_plate(&self) -> BTreeMap<(i64, i64), Vec<Polygon<f64>>> {
        self.source.pieces_by_plate()
    }

    // ----------------------------------------------------------------- project

    /// Replace the project (validated; inputs must match the open files).
    pub fn set_project(&mut self, data: &Value, base: Option<&Path>) -> Result<Value> {
        let mut p = project::validate(data, base, &self.library)?;
        let open: Vec<String> = self
            .source
            .input_paths()
            .iter()
            .map(|p| absolute(p).to_string_lossy().into_owned())
            .collect();
        let given: Vec<String> = project::inputs(&p)
            .iter()
            .map(|i| absolute(Path::new(i)).to_string_lossy().into_owned())
            .collect();
        if !given.is_empty() && given != open {
            return err("the project's inputs don't match the open files");
        }
        p["source"]["inputs"] = json!(open.iter().map(|o| json!({"path": o})).collect::<Vec<_>>());
        self.project = p;
        self.clear_results();
        self.relink();
        Ok(self.project.clone())
    }

    fn placements(&self) -> Vec<Value> {
        crate::target::placements(&self.project)
    }

    fn relink(&mut self) {
        let placements = self.placements();
        if placements.is_empty() {
            self.links.clear();
            self.unplaced.clear();
        } else {
            (self.links, self.unplaced) = assembly::relink(&placements, &self.pieces_by_plate());
        }
    }

    // ----------------------------------------------------------------- results

    /// What every target needs before it can make anything: open files, a
    /// pattern, placed pieces found, and the edition allowing the assembly.
    fn check_ready(&self) -> Result<()> {
        if self.source.is_empty() {
            return err("no sliced files are open");
        }
        if self.project["pattern"].is_null() && self.project["assembly"]["layoutSvg"].is_null() {
            return err("choose a pattern first");
        }
        // only the placements this edition patterns need to be found
        let mut placements = self.placements();
        placements.truncate(self.edition.patterned_count(placements.len()));
        if !placements.is_empty() {
            if self.project["pattern"].is_null() {
                return err("choose a pattern first");
            }
            let links = &self.links[..self.patterned_links()];
            if let Some(trouble) = assembly::problems(&placements, links) {
                return err(format!("some placed pieces can't be found: {trouble}"));
            }
        }
        self.edition
            .check_pattern(placements.len())
            .map_err(Error::Engine)?;
        if project::raise(&self.project).values().any(|&n| n > 0) {
            self.edition
                .check_pro("Raising tiles")
                .map_err(Error::Engine)?;
        }
        if project::grout_depth(&self.project) > 1 {
            self.edition
                .check_pro("Deeper grout")
                .map_err(Error::Engine)?;
        }
        if self.project["pattern"].is_object() && self.library.is_image(&self.project["pattern"]) {
            self.edition
                .check_pro("An image pattern")
                .map_err(Error::Engine)?;
        }
        Ok(())
    }

    /// Make the G-code result (see [`Session::generate_with`]).
    pub fn generate(&mut self, progress: Progress) -> Result<Value> {
        self.generate_with(project::GCODE, progress)
    }

    /// Make a target's result from the project.
    pub fn generate_with(&mut self, kind: &str, progress: Progress) -> Result<Value> {
        self.target_index(kind)?;
        self.check_ready()?;
        self.with_target(kind, |t, cx| t.generate(cx, progress))
    }

    /// Problems and warnings of the G-code result.
    pub fn check_result(&self) -> Result<Value> {
        self.check_with(project::GCODE)
    }

    /// Problems and warnings of a target's last result.
    pub fn check_with(&self, kind: &str) -> Result<Value> {
        let k = self.target_index(kind)?;
        let project = self.edition.patterned(&self.project);
        let cx = Context {
            source: &*self.source,
            project: &project,
            links: &self.links[..self.patterned_links()],
            library: &self.library,
            edition: self.edition,
            work: &self.work.0,
        };
        self.targets[k].check(&cx)
    }

    /// Write the G-code result (see [`Session::export_with`]).
    pub fn export(&mut self, paths: &[String], progress: Progress) -> Result<Value> {
        self.export_with(project::GCODE, paths, progress)
    }

    /// Make a fresh result of a target and write it to `paths`.
    pub fn export_with(
        &mut self,
        kind: &str,
        paths: &[String],
        progress: Progress,
    ) -> Result<Value> {
        self.target_index(kind)?;
        if self.source.is_empty() {
            return err("no sliced files are open");
        }
        self.with_target(kind, |t, cx| t.check_export(cx, paths))?;
        self.check_ready()?;
        self.with_target(kind, |t, cx| t.export(cx, paths, progress))
    }

    /// The rewritten G-code of a plate from the last generate, if any.
    pub fn result_lines(&self, input: usize, plate: u32) -> Option<&[String]> {
        let k = self.target_index(project::GCODE).ok()?;
        self.targets[k]
            .as_any()
            .downcast_ref::<GcodeTarget>()?
            .result_lines(input, plate)
    }

    /// Write a plate's toolpaths (original, or the G-code result's if
    /// generated) to a buffer file.
    pub fn toolpaths(
        &mut self,
        input: usize,
        plate: u32,
        from: Option<u32>,
        to: Option<u32>,
    ) -> Result<Value> {
        self.gcode_view(|g, cx| g.toolpaths(cx, input, plate, from, to))
    }

    /// Every placed piece's toolpaths moved into the assembly.
    pub fn assembled_toolpaths(&mut self) -> Result<Value> {
        self.gcode_view_on(true, |g, cx| g.assembled_toolpaths(cx))
    }

    /// Every placed piece as solid slabs moved into the assembly.
    pub fn assembled_solid(&mut self) -> Result<Value> {
        self.gcode_view_on(true, |g, cx| g.assembled_solid(cx))
    }

    /// How each placement links to the pieces now on the plates; which
    /// placements are copies of one another (the group's first placement);
    /// and where the pattern sees each placement (ADR 0014: with the pattern
    /// on each piece, not where the assembly puts it). Without Pro, the frame
    /// of the one patterned piece is worked out from it alone, as the target
    /// does; the pieces left as sliced keep their own.
    pub fn assembly_status(&self) -> Value {
        let mut frames = assembly::piece_frames(&self.edition.patterned(&self.project));
        frames.extend(
            assembly::piece_frames(&self.project)
                .into_iter()
                .skip(frames.len()),
        );
        let frames: Vec<Value> = frames
            .iter()
            .map(|f| json!({"rotation": f.rotation, "position": [f.position.0, f.position.1]}))
            .collect();
        json!({"links": self.links.iter().map(Link::to_json).collect::<Vec<_>>(),
               "unplaced": self.unplaced.iter().map(|(i, n, k)| json!({"input": i, "plate": n, "piece": k})).collect::<Vec<_>>(),
               "copies": assembly::copy_groups(&self.project),
               "frames": frames})
    }

    /// A starting assembly: every piece unturned, where it sits on its bed,
    /// with each plate after the first slid right so that plates sit side by
    /// side, [`PLATE_GAP`] apart, instead of on top of each other.
    pub fn default_placements(&self) -> Value {
        let mut out = Vec::new();
        let mut right: Option<f64> = None;
        for ((k, n), pieces) in self.pieces_by_plate() {
            let bounds = pieces
                .iter()
                .filter_map(|p| p.bounding_rect())
                .map(|r| (r.min().x, r.max().x))
                .reduce(|a, b| (a.0.min(b.0), a.1.max(b.1)));
            let dx = match (right, bounds) {
                (Some(r), Some((min_x, _))) => r + PLATE_GAP - min_x,
                _ => 0.0,
            };
            if let Some((_, max_x)) = bounds {
                right = Some(max_x + dx);
            }
            for (idx, poly) in pieces.iter().enumerate() {
                let mut pl = assembly::placement_for(k, n, idx + 1, poly, 0.0, None);
                if dx != 0.0 {
                    let x = pl["position"][0].as_f64().unwrap_or(0.0);
                    pl["position"][0] = json!(((x + dx) * 1e4).round() / 1e4);
                }
                out.push(pl);
            }
        }
        json!(out)
    }

    /// Write the assembly as a layout SVG.
    pub fn export_layout(&self, path: &Path) -> Result<Value> {
        let placements = self.placements();
        if placements.is_empty() {
            return err("the assembly has no placed pieces");
        }
        let (rot, org, _) = assembly::pattern_placement(&self.project);
        let (text, _) = assembly::placements_layout(&placements, rot, org)?;
        write_atomic(path, text.as_bytes())
            .map_err(|e| Error::Engine(format!("can't write {}: {e:?}", path.display())))?;
        Ok(json!({"path": path.to_string_lossy()}))
    }

    /// Open a project file and the sliced files it names.
    pub fn load_project(&mut self, path: &Path, progress: Progress) -> Result<Value> {
        let p = project::load(path, &self.library)?;
        self.open(&project::inputs(&p), progress)?;
        self.set_project(&p, None)
    }

    pub fn save_project(&self, path: &Path) -> Result<Value> {
        project::save(&self.project, path, &self.library)?;
        Ok(json!({"path": path.to_string_lossy()}))
    }

    /// Swap the sliced files under the current project (after re-slicing) and
    /// keep everything else. One path per open input, in the same order.
    /// Placements are re-linked to the new pieces and, where found, take the
    /// new piece's number and outline; surfaces left as sliced are matched to
    /// the nearest top surface on the same plate within half a millimetre.
    /// On failure nothing changes.
    pub fn replace_inputs(&mut self, paths: &[String], progress: Progress) -> Result<Value> {
        let open = self.source.input_paths().len();
        if paths.len() != open {
            return err(format!("give one sliced file for each of the {open} open"));
        }
        let mut old_surfaces: BTreeMap<String, (usize, u32, f64)> = BTreeMap::new();
        for ((k, n), list) in self.source.surfaces_by_plate() {
            for (id, z) in list {
                old_surfaces.insert(id, (k, n, z));
            }
        }
        let mut project = self.project.clone();
        let old_source = std::mem::replace(&mut self.source, source::empty());
        let old_project = self.project.clone();
        let attempt = (|| -> Result<()> {
            self.open(paths, progress)?;
            if self.source.input_paths().len() != paths.len() {
                return err("each sliced file can only be given once");
            }
            project["source"]["inputs"] = json!(paths
                .iter()
                .map(|p| json!({"path": absolute(Path::new(p)).to_string_lossy()}))
                .collect::<Vec<_>>());
            self.set_project(&project, None)?;
            Ok(())
        })();
        if let Err(e) = attempt {
            self.source = old_source;
            self.project = old_project;
            self.clear_results();
            self.relink();
            return Err(e);
        }

        // Placements found again take the new piece's number and outline.
        let pieces = self.pieces_by_plate();
        let mut counts = Map::new();
        for s in ["ok", "moved", "ambiguous", "missing"] {
            counts.insert(s.into(), json!(0));
        }
        let links = self.links.clone();
        if let Some(pls) = self.project["assembly"]["placements"].as_array_mut() {
            for (pl, link) in pls.iter_mut().zip(&links) {
                counts[link.status] = json!(counts[link.status].as_i64().unwrap_or(0) + 1);
                if let (true, Some(piece)) = (matches!(link.status, "ok" | "moved"), link.piece) {
                    let key = (
                        pl["input"].as_i64().unwrap_or(0),
                        pl["plate"].as_i64().unwrap_or(0),
                    );
                    if let Some(poly) = pieces.get(&key).and_then(|v| v.get(piece - 1)) {
                        pl["piece"] = json!(piece);
                        pl["outline"] = assembly::outline_coords(poly);
                    }
                }
            }
        }

        // Surfaces left as sliced, matched by height on the same plate.
        let new_surfaces = self.source.surfaces_by_plate();
        let (mut excluded, mut kept, mut lost): (Vec<String>, usize, usize) = (Vec::new(), 0, 0);
        let ids: Vec<String> = self.project["source"]["surfaces"]["excluded"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|v| v.as_str().map(String::from))
            .collect();
        for sid in ids {
            if new_surfaces.values().flatten().any(|(x, _)| *x == sid) {
                excluded.push(sid);
                kept += 1;
                continue;
            }
            let near = old_surfaces.get(&sid).and_then(|&(k, n, z)| {
                new_surfaces
                    .get(&(k, n))?
                    .iter()
                    .map(|(x, z2)| ((z2 - z).abs(), x.clone()))
                    .min_by(|a, b| a.0.total_cmp(&b.0))
            });
            match near {
                Some((d, x)) if d <= 0.5 => {
                    excluded.push(x);
                    kept += 1;
                }
                _ => lost += 1,
            }
        }
        excluded.sort();
        excluded.dedup();
        self.project["source"]["surfaces"]["excluded"] = json!(excluded);
        self.relink();
        Ok(json!({"session": self.describe(), "project": self.project,
                  "report": {"placements": counts, "unplaced": self.unplaced.len(),
                             "surfaces": {"kept": kept, "lost": lost}}}))
    }

    // ---------------------------------------------------------------- patterns

    fn pattern_ref(&self, r: &Value) -> Result<Value> {
        Ok(patterns::check_ref(r, &self.library)?)
    }

    /// Every pattern, provided then the user's, each with its presets.
    pub fn patterns(&self) -> Result<Value> {
        Ok(json!(self.library.list()?))
    }

    pub fn pattern_info(&self, r: &Value) -> Result<Value> {
        Ok(patterns::info(&self.pattern_ref(r)?, &self.library)?)
    }

    pub fn pattern_geometry(&self, r: &Value, directions: Option<&Value>) -> Result<Value> {
        let empty = json!({});
        let d = directions.filter(|d| !d.is_null()).unwrap_or(&empty);
        Ok(patterns::geometry(
            Some(&self.pattern_ref(r)?),
            Some(d),
            &self.library,
        )?)
    }

    /// A pattern's SVG source, form and group names, to edit (a user pattern)
    /// or start a new user pattern from (a provided one, at its values: plain
    /// numbers to turn into {{ }}).
    pub fn pattern_source(&self, r: &Value) -> Result<Value> {
        let r = self.pattern_ref(r)?;
        let id = r.get("id").and_then(Value::as_str).unwrap_or("");
        if id.starts_with(patterns::USER) {
            let d = self.library.definition(id)?;
            let source = d["source"].as_str().unwrap_or("");
            let src = patterns::definition_of(source, d["name"].as_str().unwrap_or(""))?;
            return Ok(
                json!({"name": d["name"], "source": source, "form": d["form"], "groups": src.groups}),
            );
        }
        if let Some(file) = r.get("file").and_then(Value::as_str) {
            let (text, d) = patterns::read_user_svg(Path::new(file))?;
            return Ok(json!({"name": d.name, "source": text, "form": d.form, "groups": d.groups}));
        }
        let text = patterns::export(Some(&r), Some(&json!({})), None, &self.library)?;
        let text = strip_data_pattern(&text);
        let groups: Map<String, Value> = library::spec(id)
            .map_err(|e| Error::Engine(e.0))?
            .groups
            .iter()
            .map(|g| {
                (
                    g["group"].as_str().unwrap_or("").to_string(),
                    g["label"].clone(),
                )
            })
            .collect();
        Ok(
            json!({"name": patterns::name_of(&r, &self.library), "source": text,
                  "form": {"components": []}, "groups": groups}),
        )
    }

    /// A small parametric pattern to start a new user pattern from.
    pub fn pattern_starter(&self) -> Value {
        let (form, groups) = patterns::starter();
        json!({"name": "My pattern", "source": patterns::STARTER, "form": form, "groups": groups})
    }

    pub fn pattern_preview(
        &self,
        source: &str,
        form: &Value,
        values: Option<&Value>,
        directions: Option<&Value>,
    ) -> Result<Value> {
        let empty = json!({});
        let d = directions.filter(|d| !d.is_null()).unwrap_or(&empty);
        Ok(patterns::preview_source(source, form, values, Some(d))?)
    }

    pub fn save_pattern(
        &self,
        name: &str,
        source: &str,
        form: &Value,
        groups: Option<&Value>,
        id: Option<&str>,
    ) -> Result<Value> {
        Ok(self
            .library
            .save_pattern(name, source, form, groups.and_then(Value::as_object), id)?)
    }

    pub fn delete_pattern(&self, id: &str) -> Result<Value> {
        Ok(self.library.delete_pattern(id)?)
    }

    /// The installed pattern packs (ADR 0011).
    pub fn packs(&self) -> Value {
        crate::pack::list(&self.library)
    }

    /// Install a pattern pack file.
    pub fn import_pack(&self, path: &Path) -> Result<Value> {
        crate::pack::import(&self.library, path).map_err(|e| Error::Engine(e.0))
    }

    /// Remove an installed pattern pack.
    pub fn remove_pack(&self, id: &str) -> Result<Value> {
        crate::pack::remove(&self.library, id).map_err(|e| Error::Engine(e.0))
    }

    /// Write some of the user's patterns as a pattern pack file.
    pub fn export_pack(&self, path: &Path, ids: &[String], meta: &Value) -> Result<Value> {
        crate::pack::export(&self.library, path, ids, meta).map_err(|e| Error::Engine(e.0))
    }

    pub fn import_pattern(&self, path: &Path) -> Result<Value> {
        Ok(self.library.import_svg(path, None)?)
    }

    /// An image added to the library as an image pattern (ADR 0015; Pro).
    pub fn import_image(&self, path: &Path) -> Result<Value> {
        self.edition
            .check_pro("An image pattern")
            .map_err(Error::Engine)?;
        Ok(self.library.import_image(path, None)?)
    }

    pub fn export_pattern(
        &self,
        r: &Value,
        directions: Option<&Value>,
        path: &Path,
        title: Option<&str>,
    ) -> Result<Value> {
        let empty = json!({});
        let d = directions.filter(|d| !d.is_null()).unwrap_or(&empty);
        let text = patterns::export(Some(&self.pattern_ref(r)?), Some(d), title, &self.library)?;
        write_atomic(path, text.as_bytes())
            .map_err(|e| Error::Engine(format!("can't write {}: {e:?}", path.display())))?;
        Ok(json!({"path": path.to_string_lossy()}))
    }

    pub fn save_preset(
        &self,
        pattern: &str,
        name: &str,
        values: &Value,
        directions: Option<&Value>,
        id: Option<&str>,
    ) -> Result<Value> {
        Ok(self
            .library
            .save_preset(pattern, name, Some(values), directions, id)?)
    }

    pub fn rename_preset(&self, pattern: &str, id: &str, name: &str) -> Result<Value> {
        Ok(self.library.rename_preset(pattern, id, name)?)
    }

    pub fn delete_preset(&self, pattern: &str, id: &str) -> Result<Value> {
        Ok(self.library.delete_preset(pattern, id)?)
    }

    // ------------------------------------------------------------------ result

    // -------------------------------------------------------- assembled view
}

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Standard base64 with padding.
pub fn base64(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for c in data.chunks(3) {
        let b = [c[0], *c.get(1).unwrap_or(&0), *c.get(2).unwrap_or(&0)];
        let n = (b[0] as u32) << 16 | (b[1] as u32) << 8 | b[2] as u32;
        for k in 0..4 {
            if k <= c.len() {
                out.push(B64[(n >> (18 - 6 * k) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// `\s*data-pattern="..."` removed (a provided pattern's source to start from).
fn strip_data_pattern(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(i) = rest.find("data-pattern=\"") {
        let head = rest[..i].trim_end();
        out.push_str(head);
        let after = &rest[i + "data-pattern=\"".len()..];
        match after.find('"') {
            Some(j) => rest = &after[j + 1..],
            None => {
                out.push_str(&rest[head.len()..]);
                rest = "";
            }
        }
    }
    out.push_str(rest);
    out
}

/// What a project file holds, for the recent-projects list: inputs and a thumbnail.
pub fn project_summary(path: &Path, lib: &Library) -> Result<Value> {
    let p = project::load(path, lib)?;
    let inputs = project::inputs(&p);
    let thumb = inputs
        .first()
        .and_then(|i| source::thumbnail(Path::new(i), 1));
    Ok(
        json!({"inputs": inputs, "exists": inputs.iter().map(|i| Path::new(i).exists()).collect::<Vec<_>>(),
              "thumbnail": thumb.map(|png| format!("data:image/png;base64,{}", base64(&png)))}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_pads() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn data_pattern_is_stripped() {
        assert_eq!(
            strip_data_pattern("<svg a=\"1\"\n  data-pattern=\"{&quot;id&quot;}\" b=\"2\">"),
            "<svg a=\"1\" b=\"2\">"
        );
    }
}
