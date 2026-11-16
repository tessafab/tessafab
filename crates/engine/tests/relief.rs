//! Relief, the app's Depth (ADR 0010, M22a): the pattern carried into the solid
//! layers under the top, grout and all, and over a patterned bottom.

use serde_json::json;
use std::path::PathBuf;
use tessafab_core::edition::Edition;
use tessafab_core::session::{no_progress, Error, Session};

fn square(edition: Edition, grout_depth: u64) -> Session {
    square_faces(edition, grout_depth, json!(["top"]))
}

fn square_faces(edition: Edition, grout_depth: u64, faces: serde_json::Value) -> Session {
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
                "source": {"surfaces": {"faces": faces}},
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

/// The lines of one layer (0: before the first layer change).
fn layer_lines(lines: &[String], layer: usize) -> Vec<String> {
    let mut k = 0;
    let mut out = Vec::new();
    for l in lines {
        if l.starts_with("; CHANGE_LAYER") || l.starts_with(";LAYER_CHANGE") {
            k += 1;
        }
        if k == layer {
            out.push(l.clone());
        }
    }
    out
}

#[test]
fn a_patterned_bottom_takes_the_relief_depth() {
    let both = || json!(["top", "bottom"]);
    let mut flat = square_faces(Edition::Pro, 1, both());
    let r = flat.generate(&mut no_progress).unwrap();
    let log = r["plates"][0]["log"].as_str().unwrap();
    assert!(log.contains("layer 1: patterned bottom surface"), "{log}");
    assert!(!log.contains("bottom relief"), "{log}");
    let flat_lines = flat.result_lines(0, 1).unwrap().to_vec();

    let mut deep = square_faces(Edition::Pro, 2, both());
    let r = deep.generate(&mut no_progress).unwrap();
    let log = r["plates"][0]["log"].as_str().unwrap();
    let relief: Vec<&str> = log
        .lines()
        .filter(|l| l.contains(": bottom relief "))
        .collect();
    assert_eq!(relief.len(), 1, "{log}");
    assert!(relief[0].starts_with("layer 2: "), "{log}");
    assert_eq!(deep.check_result().unwrap()["ok"], true);
    let deep_lines = deep.result_lines(0, 1).unwrap();
    // the first layer is patterned alike; the second is redrawn; the third is as sliced
    assert_eq!(layer_lines(deep_lines, 1), layer_lines(&flat_lines, 1));
    assert_ne!(layer_lines(deep_lines, 2), layer_lines(&flat_lines, 2));
    assert_eq!(layer_lines(deep_lines, 3), layer_lines(&flat_lines, 3));

    // deeper than the bottom's solid layers: patterned up to them, and said
    let mut s = square_faces(Edition::Pro, 10, both());
    let r = s.generate(&mut no_progress).unwrap();
    let log = r["plates"][0]["log"].as_str().unwrap();
    assert!(
        log.contains("over the bottom isn't solid; the bottom pattern stops there"),
        "{log}"
    );
    assert_eq!(s.check_result().unwrap()["ok"], true);
}

#[test]
fn deeper_than_the_shell_is_reported_and_free_is_refused() {
    let mut s = square(Edition::Pro, 10);
    let r = s.generate(&mut no_progress).unwrap();
    let log = r["plates"][0]["log"].as_str().unwrap();
    assert!(
        log.contains("depth: pattern depth 10 is deeper than the slicer's"),
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
