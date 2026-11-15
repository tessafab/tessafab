# The engine

The pattern engine reads a plate's sliced G-code, rebuilds its top surfaces,
adds rims, fills them with a placed pattern and writes the G-code back.

```
crates/pattern/  tessafab-pattern: patterns and assemblies, no file format
crates/engine/   tessafab-core: the G-code engine, built on crates/pattern
crates/server/   tessafab-engine: the engine server (JSON-RPC over stdio)
crates/cli/      tessafab-cli: the `tessafab` command line and slicer hook
crates/dev/      pp-dev: a developer tool (expressions, SVG reading, slicer settings)
```

`crates/pattern/` holds what doesn't depend on any file format or craft: SVG
drawings, the provided and user patterns, expressions, forms, geometry, the
assembly's geometry and the patterned assembly with its fill (the lines inside
the tiles, `fill.rs`), which every target reads. `tessafab-core` re-exports
these modules under its own paths (`tessafab_core::svg`, ...). Nothing in
`crates/pattern/` may use G-code code, and its dependencies are an allowed
list (`crates/pattern/tests/boundary.rs` checks both).

In `tessafab-core`, the session (`session.rs`) holds only what belongs to no
craft: the project, its links to the open pieces, the pattern library and the
edition's limits. Where pieces come from is a `Source` (`source.rs`; today
`SlicedSource` in `sliced.rs`), and what is made from them a `Target`
(`target.rs`: `GcodeTarget` in `gcode_target.rs`, which every session has).
Other targets are added at build time with `Session::add_target`
(`crates/engine/tests/target.rs` composes a test one); the RPC lists them
(`targets`) and `generate`, `checkResult` and `export` take a `target`
(G-code when left out).

Built from this source, the engine patterns one piece on one plate
(`edition.rs`: `Edition::Source`). TessaFab Pro builds on these crates' public
API to add assemblies of many pieces and plates, image patterns and more; none
of that code is here, and nothing here may refer to it
(`crates/engine/tests/boundary.rs` checks).

## Building and testing

Rust stable via rustup (`rustup default stable`).

```sh
cargo build --release
cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt --check
```

- `target/release/tessafab-engine serve` is the engine server (JSON-RPC 2.0
  over stdio; only protocol messages go to stdout).
- `target/release/tessafab` is the command-line tool and slicer hook:
  `tessafab --project FILE [-o OUT] [-q] GCODE`. It rewrites one sliced plate
  (`.gcode` or `.bgcode`) with a saved project's pattern, in place unless `-o`
  is given, so it can be a slicer's post-processing script (Bambu Studio,
  OrcaSlicer and PrusaSlicer pass the output file last). On any problem the
  G-code is left unchanged and the exit status is not 0.
- `target/release/pp-dev` is a developer tool for regression checks.

## Geometry notes

Output is checked against what is actually right (exact geometry, what the
slicer printed, what would print well), not only against earlier output. Some
choices that came from such checks (most in `crates/pattern/src/geom.rs`):

- i_overlay's offsets leave 1e-8 mm edges behind; offsetting such an inset
  again (the concentric rings) took those edges' directions from float noise,
  so rings came out short and a tile's innermost rings were lost.
  `geom::buffer` drops edges under 1e-6 mm first (`offset_tests` holds the tile).
- i_overlay bevels every turn under 5 degrees, even with a mitre join, so each
  offset doubled a curve's vertices and rims along curved edges became runs of
  0.02 mm moves. `geom::buffer` simplifies its input (to 2 microns) and puts
  bevels back to mitre points. Only edges on a turn under 5 degrees count as
  bevels: deep inside a hexagon, a real 0.3 mm edge was once taken for one,
  and the innermost ring came out a pentagon.
- geo strokes every line as an open path, so a closed grout outline got two
  square caps at its first point instead of a mitre, notching the tiles beside
  it. `geom::stroke_ring` strokes closed outlines as rings.
- A ring wholly inside the area is kept whole, instead of being clipped into
  pieces wherever the overlay snapped a vertex.
- Gap closing drops fill that would land over the open middle of a small hole
  instead of giving up closing for the layer, and simplified outlines are kept
  valid.
- JSON numbers are parsed exactly (serde_json's `float_roundtrip`); the default
  parser could be 1 ULP off.

Project files: a project whose `patternPlacement`, `surfaces` or `colour` is
not an object gets an error message; a fractional loop count
(`"border": 2.5`) and `"schemaVersion": true` are refused when the project is
read. The engine writes schema 3 and still reads and upgrades version-1 and
version-2 projects (`crates/engine/src/project.rs` and its tests).
