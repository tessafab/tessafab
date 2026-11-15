//! Relief (ADR 0010, M22a): grout cut into the solid layers under the top.

use serde_json::json;
use std::path::PathBuf;
use tessafab_core::edition::Edition;
use tessafab_core::session::{no_progress, Error, Session};

fn square(edition: Edition, grout_depth: u64) -> Session {
    let ex = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples");
    let mut s = Session::new(None);
    s.edition = edition;
    s.open(
        &[ex.join("square.gcode.3mf").to_string_lossy().to_string()],
        &mut no_progress,
    )
    .unwrap();
    let placements = s.default_placements();
    s.set_project(
        &json!({"schemaVersion": 3, "assembly": {"placements": placements},
                "pattern": {"id": "running_bond", "values": {"grout": 1.0}},
                "targets": {"gcode": {"relief": {"groutDepth": grout_depth}}}}),
        None,
    )
    .unwrap();
    s
}

/// Plastic (E) extruded on each layer, by layer number.
fn e_by_layer(lines: &[String]) -> Vec<f64> {
    let mut out = vec![0.0];
    for l in lines {
        if l.starts_with("; CHANGE_LAYER") || l.starts_with(";LAYER_CHANGE") {
            out.push(0.0);
        }
        if l.starts_with("G1 ") || l.starts_with("G2 ") || l.starts_with("G3 ") {
            if let Some(e) = l.split_whitespace().find_map(|w| w.strip_prefix('E')) {
                if let Ok(v) = e.parse::<f64>() {
                    if v > 0.0 {
                        *out.last_mut().unwrap() += v;
                    }
                }
            }
        }
    }
    out
}

#[test]
fn grout_is_cut_into_the_layers_under_the_top() {
    let mut flat = square(Edition::Pro, 1);
    flat.generate(&mut no_progress).unwrap();
    let flat_e = e_by_layer(flat.result_lines(0, 1).unwrap());

    let mut deep = square(Edition::Pro, 3);
    let r = deep.generate(&mut no_progress).unwrap();
    let log = r["plates"][0]["log"].as_str().unwrap();
    let grout_layers: Vec<&str> = log.lines().filter(|l| l.contains(": grout ")).collect();
    assert_eq!(grout_layers.len(), 2, "{log}");
    assert_eq!(deep.check_result().unwrap()["ok"], true);
    let deep_e = e_by_layer(deep.result_lines(0, 1).unwrap());
    // the two layers under the top lose their grout's plastic; the rest is as flat
    let less: Vec<usize> = (0..flat_e.len())
        .filter(|&k| deep_e[k] < flat_e[k] - 1e-3)
        .collect();
    assert_eq!(less.len(), 2, "layers with less plastic: {less:?}");
    for k in 0..flat_e.len() {
        if !less.contains(&k) {
            assert!((deep_e[k] - flat_e[k]).abs() < 1e-3, "layer {k}");
        }
    }
}

#[test]
fn deeper_than_the_shell_is_reported_and_free_is_refused() {
    let mut s = square(Edition::Pro, 10);
    let r = s.generate(&mut no_progress).unwrap();
    let log = r["plates"][0]["log"].as_str().unwrap();
    assert!(
        log.contains("relief: grout depth 10 is deeper than the slicer's"),
        "{log}"
    );

    let mut s = square(Edition::Community, 3);
    assert_eq!(
        s.generate(&mut no_progress).unwrap_err(),
        Error::Engine("Deeper grout is part of TessaFab Pro (or its 14-day trial)".into())
    );
    // the default (the top only) is in every edition
    let mut s = square(Edition::Community, 1);
    s.generate(&mut no_progress).unwrap();
}

// ---------------------------------------------------------------- raised tiles (M22b)

fn raised(edition: Edition, layers: u64) -> Session {
    let mut s = square(edition, 1);
    let group =
        s.pattern_info(&json!({"id": "running_bond"})).unwrap()["groups"][0]["group"].clone();
    let mut p = s.project.clone();
    p["targets"]["gcode"]["relief"]["raise"] = json!({group.as_str().unwrap(): layers});
    s.set_project(&p, None).unwrap();
    s
}

#[test]
fn raised_tiles_add_layers_and_the_nozzle_never_comes_back_down() {
    let mut s = raised(Edition::Pro, 3);
    let r = s.generate(&mut no_progress).unwrap();
    let log = r["plates"][0]["log"].as_str().unwrap();
    assert!(
        log.contains("relief: 3 layer(s) of raised tiles added above Z 3.000, up to Z 3.600"),
        "{log}"
    );
    assert_eq!(s.check_result().unwrap()["ok"], true);
    let lines = s.result_lines(0, 1).unwrap();
    let changes = |ls: &[String]| {
        ls.iter()
            .filter(|l| l.starts_with("; CHANGE_LAYER"))
            .count()
    };
    let mut flat = square(Edition::Pro, 1);
    flat.generate(&mut no_progress).unwrap();
    assert_eq!(
        changes(lines),
        changes(flat.result_lines(0, 1).unwrap()) + 3
    );
    // after the raised layers, every absolute Z move (arcs too) stays at or above the new top
    let end = lines
        .iter()
        .position(|l| l.contains("tessafab: end of raised tiles"))
        .unwrap();
    let mut abs = true;
    for l in &lines[end..] {
        let code = l.split(';').next().unwrap_or("").trim();
        abs = match code.split_whitespace().next() {
            Some("G90") => true,
            Some("G91") => false,
            _ => abs,
        };
        let is_move = ["G0 ", "G1 ", "G2 ", "G3 "]
            .iter()
            .any(|c| code.starts_with(c));
        if abs && is_move {
            if let Some(z) = code.split_whitespace().find_map(|w| w.strip_prefix('Z')) {
                let z: f64 = z.parse().unwrap();
                assert!(z >= 3.6 - 1e-6, "the nozzle comes back down to Z {z}: {l}");
            }
        }
    }
}

#[test]
fn raising_is_pro_and_checked() {
    let mut s = raised(Edition::Community, 2);
    assert_eq!(
        s.generate(&mut no_progress).unwrap_err(),
        Error::Engine("Raising tiles is part of TessaFab Pro (or its 14-day trial)".into())
    );
    let mut s = square(Edition::Pro, 1);
    let mut p = s.project.clone();
    p["targets"]["gcode"]["relief"]["raise"] = json!({"#ff0000": 99});
    assert_eq!(
        s.set_project(&p, None).unwrap_err(),
        Error::Engine(
            "targets.gcode.relief.raise must map tile groups to layers, from 0 to 20".into()
        )
    );
}
