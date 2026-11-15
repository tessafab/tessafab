//! Bottom-surface patterns (M20): the face the pieces lie on is patterned
//! like the top, in Bambu Studio and PrusaSlicer slices, and the printer ends
//! every layer where the original does.

use serde_json::{json, Value};
use std::path::PathBuf;
use tessafab_core::edition::Edition;
use tessafab_core::session::{no_progress, Session};

fn repo() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn session(file: PathBuf, faces: Value) -> Session {
    let mut s = Session::new(None);
    s.edition = Edition::Pro;
    s.open(&[file.to_string_lossy().to_string()], &mut no_progress)
        .unwrap();
    let placements = s.default_placements();
    s.set_project(
        &json!({"schemaVersion": 3, "assembly": {"placements": placements},
                "pattern": {"id": "herringbone"},
                "source": {"surfaces": {"faces": faces}}}),
        None,
    )
    .unwrap();
    s
}

/// The plate's log, from a fresh generate.
fn generate(s: &mut Session) -> (Value, String) {
    let r = s.generate(&mut no_progress).unwrap();
    let log = r["plates"][0]["log"].as_str().unwrap_or("").to_string();
    (r, log)
}

fn check(file: PathBuf) {
    // the bottom only: the top is left as sliced, by choice
    let mut s = session(file.clone(), json!(["bottom"]));
    let described = s.describe();
    let bottoms = described["inputs"][0]["plates"][0]["bottomSurfaces"]
        .as_array()
        .unwrap()
        .clone();
    assert_eq!(bottoms.len(), 1, "{}", file.display());
    assert!(bottoms[0]["id"].as_str().unwrap().ends_with("/bottom"));
    let (_, log) = generate(&mut s);
    assert!(log.contains("patterned bottom surface"), "{log}");
    let top_patterned = |log: &str| {
        log.lines()
            .any(|l| l.contains(": patterned ") && !l.contains("bottom"))
    };
    assert!(!top_patterned(&log), "{log}");
    let c = s.check_result().unwrap();
    assert_eq!(c["ok"], true, "{c}");
    // the top isn't a warning: leaving it was the user's choice
    assert!(c["warnings"].as_array().unwrap().is_empty(), "{c}");
    let first = s
        .result_lines(0, 1)
        .unwrap()
        .iter()
        .find(|l| l.starts_with("; postprocessed by TessaFab"))
        .cloned();
    assert!(first.unwrap().contains(" bottom, "));

    // both faces
    let mut s = session(file.clone(), json!(["top", "bottom"]));
    let (_, log) = generate(&mut s);
    assert!(log.contains("patterned bottom surface"), "{log}");
    assert!(top_patterned(&log), "{log}");
    assert_eq!(s.check_result().unwrap()["ok"], true);

    // a bottom surface left out by id stays as sliced
    let id = bottoms[0]["id"].clone();
    let mut p = s.project.clone();
    p["source"]["surfaces"]["excluded"] = json!([id]);
    s.set_project(&p, None).unwrap();
    let (_, log) = generate(&mut s);
    assert!(
        log.contains("kept original bottom surface (not selected)"),
        "{log}"
    );
}

#[test]
fn bambu_square_and_tiles() {
    check(repo().join("examples/square.gcode.3mf"));
    check(repo().join("examples/tiles.gcode.3mf"));
}

#[test]
fn prusa_tiles() {
    check(repo().join("fixtures/prusa/tiles_mini.gcode"));
}
