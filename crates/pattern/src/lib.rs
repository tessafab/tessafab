//! TessaFab patterns and assemblies: tile drawings, the provided and user
//! patterns with their parameters, and where each piece sits once assembled.
//!
//! Nothing here knows about any file format or craft (G-code, embroidery,
//! laser): sources and targets build on this crate, never the other way
//! round (ADR 0008; `tests/boundary.rs` checks).

#![allow(clippy::type_complexity, clippy::too_many_arguments)]

pub mod assembly;
pub mod direction;
pub mod drawing;
pub mod expr;
pub mod fill;
pub mod form;
pub mod geom;
pub mod library;
pub mod patterns;
pub mod svg;
