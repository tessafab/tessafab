//! Spike S1 (ADR 0002), as the Python test_slicer_macro.py runs it: expand
//! Bambu's change_filament_gcode ourselves and compare with what Bambu Studio
//! itself wrote, tool change by tool change, in every multicolour fixture
//! (fixtures/multicolour, sliced by Bambu Studio 02.09.01.70).
//!
//! Comments, blank lines, spacing and Bambu's `M73` progress lines are
//! ignored; everything else must match. Our expansion goes through Bambu's
//! cooling-pass rule (`drop_redundant_feedrates`) first, as Bambu's does.

use std::path::PathBuf;
use tessafab_core::slicer::{read_header_config, Value};
use tessafab_core::slicer_macro::{self as sm, ChangeAt, ToolChange};

fn fixtures() -> Vec<PathBuf> {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/multicolour");
    let mut v: Vec<PathBuf> = std::fs::read_dir(&dir)
        .expect("fixtures")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "gcode"))
        .collect();
    v.sort();
    v
}

fn ok_line(lines: &[String], i: usize) -> bool {
    sm::meaningful(&lines[i]).is_some_and(|m| !m.starts_with("M73"))
}

/// Meaningful lines (no comments, no M73) around the T line, and the first index.
fn bambu_lines(
    lines: &[String],
    tc: &ToolChange,
    n_before: usize,
    n_after: usize,
) -> (Vec<String>, usize) {
    let mut before = Vec::new();
    let mut i = tc.line as i64 - 1;
    while before.len() < n_before && i >= 0 {
        if ok_line(lines, i as usize) {
            before.push(i as usize);
        }
        i -= 1;
    }
    before.reverse();
    let mut after = Vec::new();
    let mut i = tc.line + 1;
    while after.len() < n_after && i < lines.len() {
        if ok_line(lines, i) {
            after.push(i);
        }
        i += 1;
    }
    let first = before.first().copied().unwrap_or(tc.line);
    let idx: Vec<usize> = before.into_iter().chain([tc.line]).chain(after).collect();
    (
        idx.iter()
            .map(|&j| sm::meaningful(&lines[j]).unwrap())
            .collect(),
        first,
    )
}

#[test]
fn expansion_matches_bambu() {
    let mut checked = 0;
    let mut failures = Vec::new();
    for f in fixtures() {
        let text = std::fs::read_to_string(&f).unwrap();
        let lines: Vec<String> = text.split('\n').map(String::from).collect();
        let cfg = read_header_config(&lines);
        let changes = sm::find_tool_changes(&lines);
        let boxes = sm::object_bboxes_from_gcode(&lines);
        let macro_text = cfg
            .get("change_filament_gcode")
            .and_then(Value::as_str)
            .unwrap()
            .to_string();
        let tower = cfg
            .get("enable_prime_tower")
            .and_then(Value::as_f64)
            .unwrap_or(0.0)
            != 0.0;
        let rel_e = cfg
            .get("use_relative_e_distances")
            .and_then(Value::as_f64)
            .unwrap_or(0.0)
            != 0.0;
        for tc in &changes {
            let at = ChangeAt {
                layer_z: tc.layer_z,
                max_layer_z: tc.max_layer_z,
                toolchange_count: tc.count,
                first_layer: tc.layer_index == 0,
                wipe_tower: tower,
                layer_num: Some(tc.layer_index),
                object_bboxes: Some(boxes.clone()),
                position: None,
            };
            let vars = sm::toolchange_variables(&cfg, tc.prev, tc.next, &at);
            let name = f.file_name().unwrap().to_string_lossy().to_string();
            let out = match sm::expand(&macro_text, &vars, Some(&cfg), tc.next as i64) {
                Ok(o) => o,
                Err(e) => {
                    failures.push(format!("{name} #{}: {}", tc.count, e.message));
                    continue;
                }
            };
            let raw: Vec<&str> = out.split('\n').collect();
            let t_line = format!("T{}", tc.next);
            let mut k = raw
                .iter()
                .filter_map(|l| sm::meaningful(l))
                .position(|m| m == t_line)
                .unwrap();
            let mut ours: Vec<String> = Vec::new();
            for _ in 0..3 {
                // the feedrate pass may drop lines before T; settle the window
                let (_, start) = bambu_lines(&lines, tc, k, 0);
                let mut st = sm::feedrate_state_before(&lines, start, tc.layer_start, rel_e);
                ours = sm::drop_redundant_feedrates(&raw, &mut st)
                    .iter()
                    .filter_map(|l| sm::meaningful(l))
                    .collect();
                let k2 = ours.iter().position(|m| *m == t_line).unwrap();
                if k2 == k {
                    break;
                }
                k = k2;
            }
            let (bambu, _) = bambu_lines(&lines, tc, k, ours.len() - k - 1);
            if ours != bambu {
                let first = ours
                    .iter()
                    .zip(&bambu)
                    .position(|(a, b)| a != b)
                    .unwrap_or(ours.len().min(bambu.len()));
                failures.push(format!(
                    "{name} #{} (T{}->T{}): first difference at line {first}: ours {:?}, bambu {:?}",
                    tc.count,
                    tc.prev,
                    tc.next,
                    ours.get(first),
                    bambu.get(first)
                ));
            }
            checked += 1;
        }
    }
    assert!(checked > 20, "only {checked} tool changes found");
    assert!(
        failures.is_empty(),
        "{} of {checked} differ:\n{}",
        failures.len(),
        failures.join("\n")
    );
}
