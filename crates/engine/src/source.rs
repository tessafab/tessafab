//! Sources: where a project's pieces come from (ADR 0008). The session works
//! through [`Source`]; the kinds are chosen here, at build time. Today there
//! is one, sliced G-code ([`SlicedSource`]).

use crate::project;
use crate::session::{Error, Progress, Result};
use crate::sliced::{self, SlicedSource};
use geo::Polygon;
use serde_json::Value;
use std::any::Any;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Where a project's pieces come from: files the user opened, each holding
/// one or more plates (a plate groups pieces made together: one bed of a
/// print), each plate holding pieces.
pub trait Source: Any {
    /// The kind, as a project's `source.kind` names it.
    fn kind(&self) -> &'static str;
    /// The opened files, as given.
    fn input_paths(&self) -> Vec<PathBuf>;
    /// What the app is told about the opened files.
    fn describe(&self) -> Value;
    /// Every plate's pieces, by (input, plate), each in its plate's frame (mm).
    fn pieces_by_plate(&self) -> BTreeMap<(i64, i64), Vec<Polygon<f64>>>;
    /// Every plate's surfaces the user can leave out of the pattern: id and
    /// height (mm). Empty for a source whose pieces are flat.
    fn surfaces_by_plate(&self) -> BTreeMap<(usize, u32), Vec<(String, f64)>>;
    /// For a target that needs a particular source (G-code needs sliced files).
    fn as_any(&self) -> &dyn Any;

    /// Whether nothing is open.
    fn is_empty(&self) -> bool {
        self.input_paths().is_empty()
    }
}

/// Open files as a source of this kind.
pub fn open(kind: &str, paths: &[String], progress: Progress) -> Result<Box<dyn Source>> {
    match kind {
        project::SLICED => Ok(Box::new(SlicedSource::open(paths, progress)?)),
        k => Err(Error::Engine(format!(
            "this version of TessaFab can't open a {k} source"
        ))),
    }
}

/// A source with nothing open.
pub fn empty() -> Box<dyn Source> {
    Box::new(SlicedSource::default())
}

/// The picture of a file's plate, for the recent-projects list (PNG bytes).
pub fn thumbnail(path: &Path, plate: u32) -> Option<Vec<u8>> {
    sliced::thumbnail(path, plate)
}

/// `{"thumbnail": data URL or null}`.
pub fn thumbnail_json(path: &Path, plate: u32) -> Value {
    sliced::thumbnail_json(path, plate)
}
