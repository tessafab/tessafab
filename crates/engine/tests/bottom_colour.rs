//! Colour on the bottom (M20b): the face a print lies on in the tile groups'
//! filaments, the first layer alone by default, deeper on request, with tool
//! changes on those layers and the printer state unchanged at every layer.

use serde_json::json;
use std::path::PathBuf;
use tessafab_core::edition::Edition;
use tessafab_core::session::{no_progress, Session};

fn repo() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn session(bottom_depth: u64) -> Session {
    let fix = repo().join("fixtures/multicolour/x1c_single_3fil.gcode");
    let mut s = Session::new(None);
    s.edition = Edition::Pro;
    s.open(&[fix.to_string_lossy().to_string()], &mut no_progress)
        .unwrap();
    let placements = s.default_placements();
    s.set_project(
        &json!({"schemaVersion": 3, "assembly": {"placements": placements},
                "pattern": {"file": repo().join("patterns/basketweave.svg").to_string_lossy()},
                "source": {"surfaces": {"faces": ["bottom"]}},
                "targets": {"gcode": {"colour": {"mode": "ams", "depth": 1, "bottomDepth": bottom_depth,
                                                 "filaments": {"#d8b07a": 1, "#9c6a3c": 2}}}}}),
        None,
    )
    .unwrap();
    s
}

/// Tool changes (`M620 S`) per layer (counted by layer changes).
fn changes_by_layer(lines: &[String]) -> Vec<usize> {
    let mut out = vec![0];
    for l in lines {
        if l.starts_with("; CHANGE_LAYER") {
            out.push(0);
        }
        if l.starts_with("M620 S") {
            *out.last_mut().unwrap() += 1;
        }
    }
    out
}

/// The layers holding tool changes the engine added (the slicer's own, in
/// the start and end G-code, left out).
fn change_layers(s: &Session) -> Vec<usize> {
    let fix = repo().join("fixtures/multicolour/x1c_single_3fil.gcode");
    let orig: Vec<String> = std::fs::read_to_string(fix)
        .unwrap()
        .lines()
        .map(|l| format!("{l}\n"))
        .collect();
    let (a, b) = (
        changes_by_layer(&orig),
        changes_by_layer(s.result_lines(0, 1).unwrap()),
    );
    (0..b.len())
        .filter(|&k| b[k] > a.get(k).copied().unwrap_or(0))
        .collect()
}

#[test]
fn the_first_layer_takes_the_tiles_colours() {
    let mut s = session(1);
    let r = s.generate(&mut no_progress).unwrap();
    let plate = &r["plates"][0];
    assert!(
        plate["log"]
            .as_str()
            .unwrap()
            .contains("patterned bottom surface"),
        "{}",
        plate["log"]
    );
    assert!(
        plate["colour"]["changes"].as_u64().unwrap() >= 2,
        "{}",
        plate["colour"]
    );
    assert_eq!(s.check_result().unwrap()["ok"], true);
    // the tool changes are on the first layer only
    let layers = change_layers(&s);
    assert_eq!(
        layers.len(),
        1,
        "tool changes added on one layer: {layers:?}"
    );
}

#[test]
fn a_deeper_bottom_recolours_the_slicers_lines_above_the_first() {
    let mut s = session(2);
    let r = s.generate(&mut no_progress).unwrap();
    let log = r["plates"][0]["log"].as_str().unwrap();
    assert!(log.contains("bottom colour depth"), "{log}");
    assert_eq!(s.check_result().unwrap()["ok"], true);
    let layers = change_layers(&s);
    assert_eq!(
        layers.len(),
        2,
        "tool changes added on two layers: {layers:?}"
    );
    assert_eq!(layers[1], layers[0] + 1);
}
