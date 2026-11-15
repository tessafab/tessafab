//! A target composed into a session runs end to end through the same calls
//! the app makes (ADR 0008, M18). The test target here is no craft: it lays
//! the pattern over each plate's pieces through the format-neutral code
//! (`assembly::build_placement`, `fill`) and counts the fill lines of each
//! tile group, so nothing in it reads or writes G-code.

use serde_json::{json, Value};
use std::any::Any;
use std::collections::BTreeMap;
use std::path::PathBuf;
use tessafab_core::assembly;
use tessafab_core::edition::Edition;
use tessafab_core::geom;
use tessafab_core::session::{no_progress, Error, Progress, Result, Session};
use tessafab_core::target::{Context, Target};

#[derive(Default)]
struct CountLines {
    last: Option<Value>,
}

impl CountLines {
    fn count(&self, cx: &Context) -> Result<Value> {
        let mut groups: BTreeMap<String, usize> = BTreeMap::new();
        for (key, pieces) in cx.source.pieces_by_plate() {
            let mut patterned = assembly::build_placement(
                cx.project,
                cx.links,
                &pieces,
                key,
                cx.library,
                None,
                &mut |_| {},
            )
            .map_err(|e| Error::Engine(e.0))?;
            let all = geom::union_all(&pieces);
            let (paths, _) = patterned.paths(&all, 0.5, 0.3, false);
            for (_, _, group) in paths {
                *groups.entry(group).or_default() += 1;
            }
        }
        Ok(json!({"groups": groups}))
    }
}

impl Target for CountLines {
    fn kind(&self) -> &'static str {
        "count"
    }
    fn info(&self) -> Value {
        json!({"kind": "count", "name": "Line count", "extensions": ["json"], "about": "test"})
    }
    fn clear(&mut self) {
        self.last = None;
    }
    fn generate(&mut self, cx: &Context, _progress: Progress) -> Result<Value> {
        let v = self.count(cx)?;
        self.last = Some(v.clone());
        Ok(v)
    }
    fn check(&self, _cx: &Context) -> Result<Value> {
        match &self.last {
            Some(v) => Ok(json!({"ok": true, "groups": v["groups"]})),
            None => Err(Error::Engine("there is no result yet".into())),
        }
    }
    fn check_export(&self, _cx: &Context, paths: &[String]) -> Result<()> {
        match paths.len() {
            1 => Ok(()),
            _ => Err(Error::Engine("give one path".into())),
        }
    }
    fn export(&mut self, cx: &Context, paths: &[String], progress: Progress) -> Result<Value> {
        let v = self.generate(cx, progress)?;
        std::fs::write(&paths[0], v.to_string()).map_err(|e| Error::Engine(e.to_string()))?;
        Ok(json!({"written": paths}))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

fn tiles_session() -> Session {
    let ex = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples");
    let mut s = Session::new(None);
    s.edition = Edition::Pro;
    s.add_target(Box::new(CountLines::default()));
    s.open(
        &[ex.join("tiles.gcode.3mf").to_string_lossy().to_string()],
        &mut no_progress,
    )
    .unwrap();
    let placements = s.default_placements();
    s.set_project(
        &json!({"schemaVersion": 3, "assembly": {"placements": placements},
                "pattern": {"id": "basketweave"}}),
        None,
    )
    .unwrap();
    s
}

#[test]
fn a_composed_target_runs_end_to_end() {
    let mut s = tiles_session();
    let kinds: Vec<Value> = s
        .targets()
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["kind"].clone())
        .collect();
    assert_eq!(kinds, vec![json!("gcode"), json!("count")]);

    let r = s.generate_with("count", &mut no_progress).unwrap();
    let groups = r["groups"].as_object().unwrap();
    assert!(groups.len() >= 2, "{r}");
    assert!(groups.values().all(|n| n.as_u64().unwrap() > 0), "{r}");
    assert_eq!(s.check_with("count").unwrap()["groups"], r["groups"]);

    // a new project clears every target's result
    let p = s.project.clone();
    s.set_project(&p, None).unwrap();
    assert!(s.check_with("count").is_err());

    let out = std::env::temp_dir().join(format!("tessafab-count-{}.json", std::process::id()));
    let paths = [out.to_string_lossy().to_string()];
    s.export_with("count", &paths, &mut no_progress).unwrap();
    let written: Value = serde_json::from_str(&std::fs::read_to_string(&out).unwrap()).unwrap();
    assert_eq!(written["groups"], r["groups"]);
    let _ = std::fs::remove_file(&out);
    assert_eq!(
        s.export_with("count", &[], &mut no_progress).unwrap_err(),
        Error::Engine("give one path".into())
    );

    // G-code still works alongside it
    s.generate(&mut no_progress).unwrap();
    assert!(s.result_lines(0, 1).is_some());
}

#[test]
fn the_session_checks_the_project_for_every_target() {
    let mut s = tiles_session();
    s.set_project(&json!({"schemaVersion": 3}), None).unwrap();
    assert_eq!(
        s.generate_with("count", &mut no_progress).unwrap_err(),
        Error::Engine("choose a pattern first".into())
    );
    assert_eq!(
        s.generate_with("embroidery", &mut no_progress).unwrap_err(),
        Error::Engine("this version of TessaFab can't make embroidery".into())
    );
}
