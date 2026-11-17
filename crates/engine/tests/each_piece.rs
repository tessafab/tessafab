//! The pattern on each piece (ADR 0014), end to end on the hexes example:
//! nine copies of one hexagon, turned 0, 10 and 20 degrees on their beds.
//! A probe target lays the pattern over each plate the way every target does
//! (`assembly::build_placement`) and measures each piece's fill lines.

use serde_json::{json, Value};
use std::any::Any;
use std::path::PathBuf;
use tessafab_core::assembly;
use tessafab_core::edition::Edition;
use tessafab_core::geom;
use tessafab_core::session::{no_progress, Error, Progress, Result, Session};
use tessafab_core::target::{Context, Target};

/// Per placed piece: the length of its fill lines per tile group.
struct Probe;

impl Probe {
    fn measure(cx: &Context) -> Result<Value> {
        let mut out = Vec::new();
        for (key, pieces) in cx.source.pieces_by_plate() {
            let mut pa = assembly::build_placement(
                cx.project,
                cx.links,
                &pieces,
                key,
                cx.library,
                None,
                &mut |_| {},
            )
            .map_err(|e| Error::Engine(e.0))?;
            for (k, poly) in pieces.iter().enumerate() {
                if pa.transforms[k].is_none() {
                    continue;
                }
                let (paths, _) = pa.paths(&geom::poly_mp(poly), 0.45, 0.45, false);
                let mut by = serde_json::Map::new();
                for (segs, _, group) in paths {
                    let len: f64 = segs.iter().map(|(a, b)| (b.0 - a.0).hypot(b.1 - a.1)).sum();
                    let v = by.get(&group).and_then(Value::as_f64).unwrap_or(0.0);
                    by.insert(group, json!(v + len));
                }
                out.push(json!({"plate": key.1, "piece": k + 1, "lines": by}));
            }
        }
        Ok(json!(out))
    }
}

impl Target for Probe {
    fn kind(&self) -> &'static str {
        "probe"
    }
    fn info(&self) -> Value {
        json!({"kind": "probe", "name": "Probe", "extensions": ["json"], "about": "test"})
    }
    fn clear(&mut self) {}
    fn generate(&mut self, cx: &Context, _progress: Progress) -> Result<Value> {
        Probe::measure(cx)
    }
    fn check(&self, _cx: &Context) -> Result<Value> {
        Ok(json!({"ok": true}))
    }
    fn check_export(&self, _cx: &Context, _paths: &[String]) -> Result<()> {
        Ok(())
    }
    fn export(&mut self, _cx: &Context, _paths: &[String], _progress: Progress) -> Result<Value> {
        Ok(json!({}))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

fn hexes(mode: Option<&str>) -> Session {
    hexes_in(Edition::Pro, mode)
}

fn hexes_in(edition: Edition, mode: Option<&str>) -> Session {
    let ex = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples");
    let mut s = Session::new(None);
    s.edition = edition;
    s.add_target(Box::new(Probe));
    s.open(
        &[ex.join("hexes.gcode.3mf").to_string_lossy().to_string()],
        &mut no_progress,
    )
    .unwrap();
    // every piece where it sits on its bed: copies turned 0, 10 and 20 degrees
    let placements = s.default_placements();
    let mut pp = json!({"origin": null, "rotation": null, "scale": 1.0});
    if let Some(m) = mode {
        pp["mode"] = json!(m);
        pp["seed"] = json!(3);
    }
    s.set_project(
        &json!({"schemaVersion": 3, "assembly": {"placements": placements},
                "pattern": {"id": "herringbone"}, "patternPlacement": pp}),
        None,
    )
    .unwrap();
    s
}

/// Each piece's line length per group, in a fixed order of groups.
fn lengths(r: &Value) -> Vec<Vec<f64>> {
    let rows = r.as_array().unwrap();
    let mut groups: Vec<String> = rows
        .iter()
        .flat_map(|p| p["lines"].as_object().unwrap().keys().cloned())
        .collect();
    groups.sort();
    groups.dedup();
    rows.iter()
        .map(|p| {
            groups
                .iter()
                .map(|g| p["lines"][g].as_f64().unwrap_or(0.0))
                .collect()
        })
        .collect()
}

/// The largest difference between two pieces' line lengths, as a share.
fn spread_of(rows: &[Vec<f64>]) -> f64 {
    let mut worst: f64 = 0.0;
    for a in rows {
        for b in rows {
            for (x, y) in a.iter().zip(b) {
                worst = worst.max((x - y).abs() / x.max(*y).max(1.0));
            }
        }
    }
    worst
}

#[test]
fn copies_print_alike_with_the_pattern_on_each_piece() {
    let mut s = hexes(Some("each"));
    let status = s.assembly_status();
    let copies: Vec<u64> = status["copies"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c.as_u64().unwrap())
        .collect();
    assert_eq!(copies.len(), 9);
    assert!(copies.iter().all(|&c| c == 0), "{copies:?}");
    // the pattern sees every hexagon centred, turned back alike
    for f in status["frames"].as_array().unwrap() {
        assert_eq!(f["position"], json!([0.0, 0.0]));
    }
    let each = lengths(&s.generate_with("probe", &mut no_progress).unwrap());
    assert_eq!(each.len(), 9);
    assert!(spread_of(&each) < 0.03, "{each:?}");

    // across the assembly the same pieces take different parts of the pattern
    let mut s = hexes(None);
    let across = lengths(&s.generate_with("probe", &mut no_progress).unwrap());
    assert!(spread_of(&across) > 0.05, "{across:?}");
}

#[test]
fn a_varied_pattern_differs_from_piece_to_piece() {
    let mut s = hexes(Some("vary"));
    let vary = lengths(&s.generate_with("probe", &mut no_progress).unwrap());
    assert_eq!(vary.len(), 9);
    assert!(spread_of(&vary) > 0.05, "{vary:?}");
    let frames = s.assembly_status()["frames"].clone();
    assert!(frames
        .as_array()
        .unwrap()
        .iter()
        .any(|f| f["position"] != json!([0.0, 0.0])));
}

#[test]
fn every_piece_is_patterned_top_and_bottom_in_g_code() {
    for mode in ["each", "vary"] {
        let mut s = hexes(Some(mode));
        let mut p = s.project.clone();
        p["source"]["surfaces"]["faces"] = json!(["top", "bottom"]);
        s.set_project(&p, None).unwrap();
        let r = s.generate(&mut no_progress).unwrap();
        for plate in r["plates"].as_array().unwrap() {
            assert!(plate["rewritten"].as_u64().unwrap() > 0, "{mode}: {plate}");
            assert_eq!(plate["kept"].as_u64().unwrap(), 0, "{mode}: {plate}");
        }
        let check = s.check_result().unwrap();
        assert_eq!(check["ok"], json!(true), "{mode}: {check}");
    }
}

#[test]
fn the_project_keeps_how_the_pattern_is_laid() {
    let s = hexes(Some("vary"));
    assert_eq!(s.project["patternPlacement"]["mode"], json!("vary"));
    assert_eq!(s.project["patternPlacement"]["seed"], json!(3));
    let s = hexes(Some("across"));
    assert!(s.project["patternPlacement"].get("mode").is_none());
    let mut s = hexes(None);
    let mut p = s.project.clone();
    p["patternPlacement"]["mode"] = json!("sideways");
    assert!(s.set_project(&p, None).is_err());
}

/// The hexagon tiles example's own project (seven of the same hexagon, square
/// on the bed and placed as they lie), then the hexes model (copies on the bed
/// at 0, 10 and 20 degrees): copies already square aren't turned further when
/// the pattern goes on each piece.
/// A hexagon fits itself every 60 degrees but tumbling blocks' faces only
/// every 120, so a copy turned 60 shows its light and dark faces swapped;
/// the slicer's small notches at some corners must not tip it there.
#[test]
fn copies_already_square_are_not_turned_onto_another_symmetry() {
    let ex = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples");
    let mut s = Session::new(None);
    s.edition = Edition::Pro;
    s.open(
        &[ex.join("hexagons.gcode.3mf").to_string_lossy().to_string()],
        &mut no_progress,
    )
    .unwrap();
    let mut p: Value =
        serde_json::from_str(&std::fs::read_to_string(ex.join("hexagons.tessafab.json")).unwrap())
            .unwrap();
    p["source"]["inputs"][0]["path"] = json!(ex.join("hexagons.gcode.3mf").to_string_lossy());
    p["patternPlacement"]["mode"] = json!("each");
    s.set_project(&p, None).unwrap();
    let frames = s.assembly_status()["frames"].clone();
    for (f, pl) in frames
        .as_array()
        .unwrap()
        .iter()
        .zip(p["assembly"]["placements"].as_array().unwrap())
    {
        let d =
            (f["rotation"].as_f64().unwrap() - pl["rotation"].as_f64().unwrap()).rem_euclid(360.0);
        assert!(d.min(360.0 - d) < 0.5, "{f} {}", pl["rotation"]);
    }
    // on the bed as sliced (copies at 0, 10 and 20 degrees), each is turned
    // back by its own turn, not by one 60 degrees on
    let s = hexes(Some("each"));
    let placements = s.default_placements();
    let seen: Vec<f64> = s.assembly_status()["frames"]
        .as_array()
        .unwrap()
        .iter()
        .zip(placements.as_array().unwrap())
        .map(|(f, pl)| {
            // pieces 3, 2 and 1 of each plate sit at 0, 10 and 20 degrees
            let bed = 10.0 * (3 - pl["piece"].as_i64().unwrap()) as f64;
            f["rotation"].as_f64().unwrap() + bed
        })
        .collect();
    for r in &seen {
        let d = (r - seen[0]).rem_euclid(360.0);
        assert!(d.min(360.0 - d) < 0.5, "{seen:?}");
    }
}

/// Without Pro the whole assembly stays in the project, shown in the app, and
/// only the first placement (the piece the user chose) is patterned.
#[test]
fn the_free_edition_patterns_only_the_first_placement() {
    let mut free = hexes_in(Edition::Community, None);
    assert_eq!(free.assembly_status()["links"].as_array().unwrap().len(), 9);
    assert_eq!(free.assembly_status()["frames"].as_array().unwrap().len(), 9);
    let r = free.generate_with("probe", &mut no_progress).unwrap();
    assert_eq!(r.as_array().unwrap().len(), 1);
    let free_g = free.generate(&mut no_progress).unwrap();

    let mut pro = hexes(None);
    let r = pro.generate_with("probe", &mut no_progress).unwrap();
    assert_eq!(r.as_array().unwrap().len(), 9);
    let pro_g = pro.generate(&mut no_progress).unwrap();
    let rewritten = |g: &Value| -> u64 {
        g["plates"].as_array().unwrap().iter().map(|p| p["rewritten"].as_u64().unwrap()).sum()
    };
    assert!(rewritten(&free_g) > 0);
    assert!(rewritten(&free_g) < rewritten(&pro_g));
}
