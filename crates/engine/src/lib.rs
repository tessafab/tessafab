//! TessaFab engine core: read a plate's G-code, rebuild its top surfaces,
//! add rims, fill them with a placed pattern and write the G-code back.
//!
//! This is the Community engine. Pro features live in a separate crate that
//! builds on this one; nothing here may depend on it (ADR 0004).

// The spike's modules (engine, gcode, geom) are reworked as each Python module
// is ported; until then their long signatures are allowed.
#![allow(clippy::type_complexity, clippy::too_many_arguments)]

pub mod args;
pub mod assembly;
pub mod bambu_types;
pub mod bgcode;
pub mod checks;
pub mod colour;
pub mod container;
pub mod edition;
pub mod engine;
pub mod gcode;
pub mod gcode_target;
pub mod generate;
pub mod heatshrink;
pub mod pack;
pub mod plate;
pub mod project;
pub mod relief;
pub mod rpc;
pub mod session;
pub mod sliced;
pub mod slicer;
pub mod slicer_macro;
pub mod source;
pub mod target;
pub mod toolpaths;

// The format-neutral modules (ADR 0008), under their old paths so callers
// (and the Pro plugin) keep working.
pub use tessafab_pattern::{direction, drawing, expr, fill, form, geom, library, patterns, svg};

/// The engine core's version.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
