//! Targets: what turns the patterned assembly into a file for one craft
//! (ADR 0008). The session works through [`Target`]; which targets exist is
//! chosen at build time (the servers' `main.rs`, or a test), never loaded at
//! run time. Today: G-code ([`crate::gcode_target::GcodeTarget`]).

use crate::assembly::Link;
use crate::edition::Edition;
use crate::patterns::Library;
use crate::session::{Progress, Result};
use crate::source::Source;
use serde_json::Value;
use std::any::Any;
use std::path::Path;

/// What a target works from: the open source and the project, with the
/// session's links (each placement's piece), library and edition.
pub struct Context<'a> {
    pub source: &'a dyn Source,
    /// The validated project (schema 3); the target's own settings are
    /// `project["targets"][kind]`.
    pub project: &'a Value,
    /// Per placement: what became of its piece (see `assembly::relink`).
    pub links: &'a [Link],
    pub library: &'a Library,
    pub edition: Edition,
    /// A scratch folder for buffers the app reads.
    pub work: &'a Path,
}

/// A project's placements.
pub fn placements(project: &Value) -> Vec<Value> {
    project["assembly"]["placements"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

/// Turns the patterned assembly into files for one craft.
///
/// The session checks what every target needs before calling `generate` or
/// `export` (something is open, a pattern is chosen, placed pieces are found,
/// the edition allows it), so a target only checks its own needs.
pub trait Target: Any {
    /// The kind, as a project's `targets` and the `generate`/`export` calls name it.
    fn kind(&self) -> &'static str;
    /// For the app: `{"kind", "name", "extensions", "about"}`.
    fn info(&self) -> Value;
    /// Forget the last result (the project or the source changed).
    fn clear(&mut self);
    /// Make a result from the project, kept until `clear`; a summary for the app.
    fn generate(&mut self, cx: &Context, progress: Progress) -> Result<Value>;
    /// Problems and warnings of the last result.
    fn check(&self, cx: &Context) -> Result<Value>;
    /// Whether `paths` can be written (checked before the project is).
    fn check_export(&self, cx: &Context, paths: &[String]) -> Result<()>;
    /// Make a fresh result and write it to `paths`.
    fn export(&mut self, cx: &Context, paths: &[String], progress: Progress) -> Result<Value>;
    fn as_any(&self) -> &dyn Any;
    fn as_any_mut(&mut self) -> &mut dyn Any;
}
