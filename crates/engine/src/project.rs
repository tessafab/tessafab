//! Project files: where the pieces come from, the assembly and pattern, and
//! each target's settings, saved as JSON. Started as a port of project.py.
//!
//! Schema version 3 (ADR 0008). Paths inside a project are stored relative to
//! the project file's folder and resolved to absolute paths when loaded.
//! Example:
//!
//! ```json
//! {
//!   "schemaVersion": 3,
//!   "source": {"kind": "sliced",
//!              "inputs": [{"path": "tiles.gcode.3mf"}],
//!              "surfaces": {"maxZ": null, "excluded": []}},
//!   "assembly": {"placements": [{"input": 0, "plate": 1, "piece": 1,
//!                                "outline": [[0, 0], [60, 0], [60, 60], [0, 60]],
//!                                "rotation": 90, "position": [30, 30]}],
//!                "layoutSvg": null, "assign": {}},
//!   "pattern": {"id": "honeycomb", "values": {"side": 6.0, "grout": 0.6}},
//!   "preset": "default",
//!   "patternPlacement": {"origin": null, "rotation": null, "scale": 1.0},
//!   // optional: "mode": "each" or "vary" (the pattern on each piece, ADR 0014), "seed";
//!   // "repeat": false (one copy, ADR 0015) and "align" ("center", "top-left" ...)
//!   "grout": null,
//!   "tileDirections": {},
//!   "patternSettings": {"svgPaths": "direction"},
//!   "targets": {"gcode": {"colour": {"mode": "single", "depth": 2, "filaments": {}},
//!                         "settings": {"border": 0}}}
//! }
//! ```
//!
//! `source` is where the pieces come from (today only sliced G-code: its
//! files, and which of their top surfaces are used). The assembly, pattern,
//! pattern placement, grout, line directions and `patternSettings` (how a
//! pattern drawing is read) belong to no craft. `targets` holds each target's
//! own settings by kind; a target kind this engine doesn't know is kept as it
//! is, so a project from a newer TessaFab keeps it when saved.
//!
//! The assembly is either a list of placements (pieces turned and moved into
//! place) or, for older projects, a layout SVG whose outlines the pieces are
//! slid onto. The pattern is a provided or user pattern with its parameter
//! values, a pattern SVG file (`{"file": path}`) or an image file read as a
//! pattern (`{"image": path, "values": {...}}`, ADR 0015); "preset" only records
//! which preset the values came from, for the app. Version-1 projects (with a
//! "template") and version-2 projects (with "inputs", "surfaces", "colour" and
//! "settings" at the top) are read and upgraded, as are projects with a
//! picture laid over their pattern (ADR 0009): the picture becomes the
//! pattern, placed once where it was.
//!
//! A project is kept as JSON (`serde_json::Value`): the app sends and receives
//! it as is.

use crate::args::RunArgs;
use crate::assembly;
use crate::library;
use crate::patterns::{self, Library};
use crate::slicer::{SlicerConfig, Value as SV};
use serde_json::{json, Map, Value};
use std::path::{Component, Path, PathBuf};

pub const SCHEMA_VERSION: i64 = 3;
/// The kind of source this engine reads.
pub const SLICED: &str = "sliced";
/// The G-code target's key in `targets`.
pub const GCODE: &str = "gcode";
pub const COLOUR_MODES: [&str; 3] = ["single", "ams", "manual"];

#[derive(Debug, Clone, Copy, PartialEq)]
enum Kind {
    Int,
    Float,
    Bool,
    Str,
}

/// A setting's key, its argument and type: the G-code target's settings
/// (`targets.gcode.settings`), then how a pattern drawing is read
/// (`patternSettings`).
const SETTINGS: [(&str, Kind); 20] = [
    ("border", Kind::Int),
    ("edgeRim", Kind::Int),
    ("holeRim", Kind::Int),
    ("minArea", Kind::Float),
    ("flow", Kind::Float),
    ("printSpeed", Kind::Float),
    ("travelSpeed", Kind::Float),
    ("retract", Kind::Float),
    ("retractSpeed", Kind::Float),
    ("retractThreshold", Kind::Float),
    ("zhop", Kind::Float),
    ("close", Kind::Float),
    ("wallOverlap", Kind::Float),
    ("minCoverage", Kind::Float),
    ("lineWidth", Kind::Float),
    ("layerHeight", Kind::Float),
    ("noMerge", Kind::Bool),
    // patternSettings from here
    ("svgDefaultAngle", Kind::Float),
    ("svgPaths", Kind::Str),
    ("svgSkipUnmatched", Kind::Bool),
];

/// The settings that say how a pattern drawing is read (`patternSettings`).
const PATTERN_SETTINGS: [&str; 3] = ["svgDefaultAngle", "svgPaths", "svgSkipUnmatched"];

/// Project setting -> slicer config keys that hold it (Bambu/Orca first, then PrusaSlicer).
const SLICER_KEYS: [(&str, &[&str]); 4] = [
    ("retract", &["retraction_length", "retract_length"]),
    ("retractSpeed", &["retraction_speed", "retract_speed"]),
    ("zhop", &["z_hop", "retract_lift"]),
    ("travelSpeed", &["travel_speed"]),
];

#[derive(Debug, Clone, PartialEq)]
pub struct Error(pub String);

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;

fn err<T>(m: impl Into<String>) -> Result<T> {
    Err(Error(m.into()))
}

/// An empty project with every field at its default.
pub fn new() -> Value {
    json!({
        "schemaVersion": SCHEMA_VERSION,
        "source": {"kind": SLICED, "inputs": [], "surfaces": {"faces": ["top"], "maxZ": null, "excluded": []}},
        "assembly": {"placements": [], "layoutSvg": null, "assign": {}},
        "pattern": null,
        "preset": null,
        "patternPlacement": {"origin": null, "rotation": null, "scale": 1.0},
        "grout": null,
        "tileDirections": {},
        "patternSettings": {},
        "targets": {GCODE: {"colour": {"mode": "single", "depth": 2, "bottomDepth": 1, "filaments": {}}, "settings": {}, "relief": {"groutDepth": 1, "raise": {}}}},
    })
}

/// The project's input paths (a validated project).
pub fn inputs(project: &Value) -> Vec<String> {
    project["source"]["inputs"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|i| i["path"].as_str().map(String::from))
        .collect()
}

/// Which faces a (validated) project patterns: (top, bottom).
pub fn faces(project: &Value) -> (bool, bool) {
    let f = project["source"]["surfaces"]["faces"].as_array();
    let has = |n: &str| f.is_some_and(|a| a.iter().any(|v| v == n));
    (f.is_none() || has("top"), has("bottom"))
}

/// Layers the grout is cut into, counting the top (relief; 1: the top only).
pub fn grout_depth(project: &Value) -> usize {
    gcode(project)["relief"]["groutDepth"]
        .as_u64()
        .unwrap_or(1)
        .max(1) as usize
}

/// Raised tiles: layers above the top, per tile group (relief).
pub fn raise(project: &Value) -> std::collections::BTreeMap<String, usize> {
    gcode(project)["relief"]["raise"]
        .as_object()
        .into_iter()
        .flatten()
        .filter_map(|(g, n)| Some((g.clone(), n.as_u64()? as usize)))
        .collect()
}

/// Bottom colour depth: layers from the plate in the tiles' filaments (1: the first).
pub fn bottom_depth(project: &Value) -> usize {
    gcode(project)["colour"]["bottomDepth"]
        .as_u64()
        .unwrap_or(1)
        .max(1) as usize
}

/// The G-code target's part of a (validated) project: colour and settings.
pub fn gcode(project: &Value) -> &Value {
    &project["targets"][GCODE]
}

/// A version-2 project as version 3: the inputs and top surfaces move into
/// `source`, colour and settings into `targets.gcode`, except the settings
/// that say how a pattern drawing is read, which become `patternSettings`.
/// Fields are moved as they are; checking them is left to `validate`, so a
/// wrong value names the field where it now lives.
pub fn upgrade_2(data: &Map<String, Value>) -> Result<Map<String, Value>> {
    if let Some(k) = ["patternSettings", "source", "targets"]
        .into_iter()
        .find(|k| data.contains_key(*k))
    {
        return err(format!("unknown field(s): {k}"));
    }
    let mut data = data.clone();
    let inputs = data.shift_remove("inputs");
    let surfaces = data.shift_remove("surfaces");
    let colour = data.shift_remove("colour");
    let settings = data.shift_remove("settings");
    let mut source = Map::new();
    source.insert("kind".into(), json!(SLICED));
    if let Some(v) = inputs {
        source.insert("inputs".into(), v);
    }
    if let Some(v) = surfaces {
        source.insert("surfaces".into(), v);
    }
    let mut gcode = Map::new();
    if let Some(v) = colour {
        gcode.insert("colour".into(), v);
    }
    match settings {
        Some(Value::Object(m)) => {
            let (pat, fdm): (Map<String, Value>, Map<String, Value>) = m
                .into_iter()
                .partition(|(k, _)| PATTERN_SETTINGS.contains(&k.as_str()));
            data.insert("patternSettings".into(), Value::Object(pat));
            gcode.insert("settings".into(), Value::Object(fdm));
        }
        // left for validate to name ("targets.gcode.settings must be an object")
        Some(v) => {
            gcode.insert("settings".into(), v);
        }
        None => {}
    }
    data.insert("schemaVersion".into(), json!(SCHEMA_VERSION));
    data.insert("source".into(), Value::Object(source));
    data.insert("targets".into(), json!({GCODE: gcode}));
    Ok(data)
}

fn truthy(v: &Value) -> bool {
    crate::form::truthy(Some(v))
}

/// `d.get(key) or default`.
fn get_or<'a>(m: &'a Map<String, Value>, key: &str, default: &'a Value) -> &'a Value {
    m.get(key).filter(|v| truthy(v)).unwrap_or(default)
}

fn is_int(v: &Value) -> bool {
    matches!(v, Value::Number(n) if n.is_i64() || n.is_u64())
}

fn num(v: Option<&Value>, at: &str, allow_none: bool) -> Result<Value> {
    match v {
        None | Some(Value::Null) if allow_none => Ok(Value::Null),
        Some(Value::Number(n)) => Ok(json!(n.as_f64().unwrap_or(0.0))),
        _ => err(format!("{at} must be a number")),
    }
}

fn pair(v: Option<&Value>, at: &str) -> Result<Value> {
    match v {
        None | Some(Value::Null) => Ok(Value::Null),
        Some(Value::Array(a)) if a.len() == 2 => Ok(json!([
            num(Some(&a[0]), at, false)?,
            num(Some(&a[1]), at, false)?
        ])),
        _ => err(format!("{at} must be [x, y]")),
    }
}

fn whole(v: Option<&Value>, at: &str, minimum: i64) -> Result<Value> {
    match v.and_then(Value::as_i64) {
        Some(n) if is_int(v.unwrap()) && n >= minimum => Ok(json!(n)),
        _ => err(format!("{at} must be a whole number, at least {minimum}")),
    }
}

/// `os.path.normpath`: `.` and `..` resolved without touching the disk.
pub fn normpath(p: &Path) -> PathBuf {
    let mut out: Vec<Component> = Vec::new();
    for c in p.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => match out.last() {
                Some(Component::Normal(_)) => {
                    out.pop();
                }
                Some(Component::RootDir) | Some(Component::Prefix(_)) => {}
                _ => out.push(c),
            },
            _ => out.push(c),
        }
    }
    if out.is_empty() {
        return PathBuf::from(".");
    }
    out.iter().collect()
}

fn path_str(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

/// A file path from a project, made absolute against `base`.
fn file_path(v: Option<&Value>, at: &str, base: Option<&Path>) -> Result<String> {
    let Some(s) = v.and_then(Value::as_str).filter(|s| !s.is_empty()) else {
        return err(format!("{at} must be a file path"));
    };
    match base {
        Some(b) if !Path::new(s).is_absolute() => Ok(path_str(&normpath(&b.join(s)))),
        _ => Ok(s.to_string()),
    }
}

/// `{key: value}` with `value` the default of a field Python reads with `or`.
fn object<'a>(
    m: &'a Map<String, Value>,
    key: &str,
    empty: &'a Value,
    at: &str,
) -> Result<&'a Map<String, Value>> {
    get_or(m, key, empty)
        .as_object()
        .ok_or_else(|| Error(format!("{at} must be an object")))
}

/// `templates/` became `patterns/`: the moved file, if the old one is gone.
fn moved_template(path: &str) -> Option<String> {
    for sep in ['/', '\\'] {
        for sep2 in ['/', '\\'] {
            let from = format!("{sep}templates{sep2}");
            if path.contains(&from) {
                return Some(path.replacen(&from, &format!("{sep}patterns{sep2}"), 1));
            }
        }
    }
    None
}

/// A version-1 project as version 2 (then [`upgrade_2`]): its template becomes a pattern.
///
/// Template files made from a provided pattern (and generator templates)
/// become that pattern with its values; other files stay files. The
/// templates/ folder is now patterns/, so a missing file is looked for there.
pub fn upgrade(data: &Map<String, Value>, base: Option<&Path>) -> Result<Map<String, Value>> {
    let mut data = data.clone();
    let tpl = data.shift_remove("template");
    data.insert("schemaVersion".into(), json!(2));
    data.insert("pattern".into(), Value::Null);
    match &tpl {
        Some(Value::Object(t)) if t.contains_key("generator") => {
            let g = &t["generator"];
            let Some((pid, values)) = patterns::from_old_generator(g) else {
                let family = g
                    .get("family")
                    .map(py_repr)
                    .unwrap_or_else(|| "None".into());
                return err(format!(
                    "template.generator: unknown pattern family {family}"
                ));
            };
            data.insert("pattern".into(), json!({"id": pid, "values": values}));
        }
        Some(Value::Object(t)) if t.contains_key("path") => {
            let mut path = file_path(t.get("path"), "template.path", base)?;
            if !Path::new(&path).exists() {
                if let Some(moved) = moved_template(&path).filter(|m| Path::new(m).exists()) {
                    path = moved;
                }
            }
            let meta = std::fs::read_to_string(&path)
                .ok()
                .and_then(|text| patterns::pattern_of(&text));
            match meta {
                Some(meta) => {
                    data.insert(
                        "pattern".into(),
                        json!({"id": meta["id"], "values": meta["values"]}),
                    );
                    let mut dirs = meta
                        .get("directions")
                        .and_then(Value::as_object)
                        .cloned()
                        .unwrap_or_default();
                    if let Some(own) = data
                        .get("tileDirections")
                        .filter(|v| truthy(v))
                        .and_then(Value::as_object)
                    {
                        dirs.extend(own.clone());
                    }
                    data.insert("tileDirections".into(), Value::Object(dirs));
                }
                None => {
                    data.insert("pattern".into(), json!({"file": path}));
                }
            }
        }
        None | Some(Value::Null) => {}
        Some(_) => return err("template must be {\"path\": ...} or {\"generator\": ...}"),
    }
    let ref_id = data["pattern"]
        .get("id")
        .and_then(Value::as_str)
        .map(str::to_string);
    let has_dirs = data.get("tileDirections").is_some_and(truthy);
    if let Some(pid) = ref_id.filter(|p| library::spec(p).is_ok() && !has_dirs) {
        // name the provided preset the values match, if any
        let values = library::check_values(&pid, data["pattern"].get("values")).ok();
        let provided = library::provided().map_err(|e| Error(e.0))?;
        let pattern = provided.iter().find(|p| p["id"] == pid.as_str());
        if let (Some(values), Some(pattern)) = (values, pattern) {
            let values = Value::Object(values);
            for preset in pattern["presets"].as_array().into_iter().flatten() {
                if preset["values"] == values {
                    data.insert("preset".into(), preset["id"].clone());
                    break;
                }
            }
        }
    }
    Ok(data)
}

/// Python's `repr` of a JSON value, for messages.
fn py_repr(v: &Value) -> String {
    match v {
        Value::Null => "None".into(),
        Value::String(s) => format!("'{s}'"),
        Value::Bool(b) => if *b { "True" } else { "False" }.into(),
        Value::Number(n) if n.is_f64() => library::py_float(n.as_f64().unwrap()),
        Value::Number(n) => n.to_string(),
        Value::Array(a) => format!("[{}]", a.iter().map(py_repr).collect::<Vec<_>>().join(", ")),
        Value::Object(m) => format!(
            "{{{}}}",
            m.iter()
                .map(|(k, v)| format!("'{k}': {}", py_repr(v)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

/// Check a project and return a normalised copy (defaults filled in, paths
/// made absolute against `base`).
pub fn validate(data: &Value, base: Option<&Path>, lib: &Library) -> Result<Value> {
    let Some(data) = data.as_object() else {
        return err("a project must be a JSON object");
    };
    let mut version = data.get("schemaVersion").cloned().unwrap_or(Value::Null);
    let mut upgraded = None;
    if version == json!(1) {
        upgraded = Some(upgrade(data, base)?);
        version = json!(2);
    }
    if version.as_f64() == Some(2.0) && !version.is_boolean() {
        upgraded = Some(upgrade_2(upgraded.as_ref().unwrap_or(data))?);
        version = json!(SCHEMA_VERSION);
    }
    let data = upgraded.as_ref().unwrap_or(data);
    if version.as_f64() != Some(SCHEMA_VERSION as f64) || version.is_boolean() {
        return err(format!(
            "unsupported schemaVersion {} (this engine reads 1 to {SCHEMA_VERSION})",
            py_repr(&version)
        ));
    }
    let mut p = new();
    let known = p.as_object().unwrap().clone();
    // ("picture": a picture over the pattern, ADR 0009, read below)
    let mut unknown: Vec<&String> = data
        .keys()
        .filter(|k| !known.contains_key(*k) && *k != "picture")
        .collect();
    if !unknown.is_empty() {
        unknown.sort();
        let names: Vec<&str> = unknown.iter().map(|s| s.as_str()).collect();
        return err(format!("unknown field(s): {}", names.join(", ")));
    }
    let empty_obj = json!({});
    let empty_list = json!([]);

    // source
    let src = object(data, "source", &empty_obj, "source")?;
    match src.get("kind") {
        None | Some(Value::Null) => {}
        Some(Value::String(k)) if k == SLICED => {}
        Some(Value::String(k)) => {
            return err(format!(
                "this project's pieces come from a source this version of TessaFab can't read ({k})"
            ))
        }
        Some(_) => return err("source.kind must be text, such as \"sliced\""),
    }
    let inputs = src.get("inputs").unwrap_or(&empty_list);
    let Some(inputs) = inputs.as_array() else {
        return err("source.inputs must be a list");
    };
    let mut ins = Vec::new();
    for (k, i) in inputs.iter().enumerate() {
        let path = if i.is_object() { i.get("path") } else { None };
        ins.push(json!({"path": file_path(path, &format!("source.inputs[{k}].path"), base)?}));
    }
    p["source"]["inputs"] = Value::Array(ins);
    let surf = object(src, "surfaces", &empty_obj, "source.surfaces")?;
    let excluded = surf.get("excluded").unwrap_or(&empty_list);
    match excluded.as_array() {
        Some(a) if a.iter().all(Value::is_string) => {}
        _ => return err("source.surfaces.excluded must be a list of surface ids"),
    }
    let faces = surf.get("faces").cloned().unwrap_or(json!(["top"]));
    let ok = faces.as_array().is_some_and(|a| {
        !a.is_empty()
            && a.iter().all(|v| v == "top" || v == "bottom")
            && a.iter().filter(|v| *v == "top").count() <= 1
            && a.iter().filter(|v| *v == "bottom").count() <= 1
    });
    if !ok {
        return err("source.surfaces.faces must list \"top\", \"bottom\" or both");
    }
    p["source"]["surfaces"] = json!({
        "faces": faces,
        "maxZ": num(surf.get("maxZ"), "source.surfaces.maxZ", true)?,
        "excluded": excluded,
    });
    if let Some(k) = src
        .keys()
        .find(|k| !["kind", "inputs", "surfaces"].contains(&k.as_str()))
    {
        return err(format!("unknown field source.{k}"));
    }

    // assembly
    let asm = object(data, "assembly", &empty_obj, "assembly")?;
    if let Some(l) = asm.get("layoutSvg").filter(|v| !v.is_null()) {
        p["assembly"]["layoutSvg"] = json!(file_path(Some(l), "assembly.layoutSvg", base)?);
    }
    let assign = get_or(asm, "assign", &empty_obj);
    match assign.as_object() {
        Some(m) if m.values().all(Value::is_string) => p["assembly"]["assign"] = assign.clone(),
        _ => return err("assembly.assign must map plate keys to strings like \"2:5,3:6\""),
    }
    let Some(placements) = get_or(asm, "placements", &empty_list).as_array() else {
        return err("assembly.placements must be a list");
    };
    let mut pls = Vec::new();
    for (k, pl) in placements.iter().enumerate() {
        pls.push(placement(pl, &format!("assembly.placements[{k}]"))?);
    }
    p["assembly"]["placements"] = Value::Array(pls);

    // pattern
    if let Some(r) = data.get("pattern").filter(|v| !v.is_null()) {
        let mut r = r.clone();
        for key in ["file", "image"] {
            if let Some(m) = r.as_object_mut().filter(|m| m.contains_key(key)) {
                let f = file_path(m.get(key), &format!("pattern.{key}"), base)?;
                m.insert(key.into(), json!(f));
            }
        }
        p["pattern"] = patterns::check_ref(&r, lib).map_err(|e| Error(format!("pattern: {e}")))?;
    }
    match data.get("preset") {
        None | Some(Value::Null) => {}
        Some(v @ Value::String(_)) => p["preset"] = v.clone(),
        Some(_) => return err("preset must be a preset id"),
    }
    let dirs = get_or(data, "tileDirections", &empty_obj);
    let ok = dirs.as_object().is_some_and(|m| {
        m.values()
            .all(|v| v == "rings" || matches!(v, Value::Number(_)))
    });
    if !ok {
        return err("tileDirections must map tile groups to degrees or \"rings\"");
    }
    let dirs: Map<String, Value> = dirs
        .as_object()
        .unwrap()
        .iter()
        .map(|(k, v)| {
            (
                k.clone(),
                if v == "rings" {
                    v.clone()
                } else {
                    json!(v.as_f64().unwrap())
                },
            )
        })
        .collect();
    p["tileDirections"] = Value::Object(dirs);

    // pattern placement
    let pp = object(data, "patternPlacement", &empty_obj, "patternPlacement")?;
    let scale = num(
        Some(pp.get("scale").unwrap_or(&json!(1.0))),
        "patternPlacement.scale",
        false,
    )?;
    p["patternPlacement"] = json!({
        "origin": pair(pp.get("origin"), "patternPlacement.origin")?,
        "rotation": num(pp.get("rotation"), "patternPlacement.rotation", true)?,
        "scale": scale,
    });
    if scale.as_f64().unwrap() <= 0.0 {
        return err("patternPlacement.scale must be more than 0");
    }
    // the pattern on each piece (ADR 0014); left out: across the assembly
    match pp.get("mode") {
        None | Some(Value::Null) => {}
        Some(Value::String(m)) if m == "across" => {}
        Some(Value::String(m)) if m == "each" || m == "vary" => {
            p["patternPlacement"]["mode"] = json!(m);
        }
        Some(_) => return err("patternPlacement.mode must be \"across\", \"each\" or \"vary\""),
    }
    match pp.get("seed") {
        None | Some(Value::Null) => {}
        Some(v) => match v.as_u64() {
            Some(n) => p["patternPlacement"]["seed"] = json!(n),
            None => return err("patternPlacement.seed must be a whole number, 0 or more"),
        },
    }
    // a pattern placed once (ADR 0015); left out: it repeats, centred
    match pp.get("repeat") {
        None | Some(Value::Null) | Some(Value::Bool(true)) => {}
        Some(Value::Bool(false)) => p["patternPlacement"]["repeat"] = json!(false),
        Some(_) => return err("patternPlacement.repeat must be true or false"),
    }
    match pp.get("align") {
        None | Some(Value::Null) => {}
        Some(Value::String(a)) if a == "center" => {}
        Some(Value::String(a)) if assembly::ALIGNS.contains(&a.as_str()) => {
            p["patternPlacement"]["align"] = json!(a);
        }
        Some(_) => {
            return err(format!(
                "patternPlacement.align must be one of {}",
                assembly::ALIGNS.join(", ")
            ))
        }
    }
    p["grout"] = num(data.get("grout"), "grout", true)?;

    // a picture laid over the pattern (ADR 0009) becomes the pattern (ADR 0015)
    match data.get("picture") {
        None | Some(Value::Null) => {}
        Some(Value::Object(m)) => from_picture(&mut p, m, base, lib)?,
        Some(_) => return err("picture must be an object with the picture's file"),
    }

    // pattern settings
    let ps = get_or(data, "patternSettings", &empty_obj);
    let Some(ps) = ps.as_object() else {
        return err("patternSettings must be an object");
    };
    for (key, value) in ps {
        if !PATTERN_SETTINGS.contains(&key.as_str()) {
            return err(format!("unknown pattern setting '{key}'"));
        }
        check_setting(key, value, "patternSettings")?;
        p["patternSettings"][key] = value.clone();
    }

    // targets: the G-code target is checked; others are kept as they are
    let targets = object(data, "targets", &empty_obj, "targets")?;
    for (kind, t) in targets {
        if kind != GCODE {
            p["targets"][kind] = t.clone();
        }
    }
    let gc = object(targets, GCODE, &empty_obj, "targets.gcode")?;
    if let Some(k) = gc
        .keys()
        .find(|k| !["colour", "settings", "relief"].contains(&k.as_str()))
    {
        return err(format!("unknown field targets.gcode.{k}"));
    }
    let col = object(gc, "colour", &empty_obj, "targets.gcode.colour")?;
    let mode = col.get("mode").cloned().unwrap_or(json!("single"));
    if !mode.as_str().is_some_and(|m| COLOUR_MODES.contains(&m)) {
        return err(format!(
            "targets.gcode.colour.mode must be one of {}",
            COLOUR_MODES.join(", ")
        ));
    }
    let depth = col.get("depth").cloned().unwrap_or(json!(2));
    if !is_int(&depth) || depth.as_i64().is_none_or(|d| d < 1) {
        return err("targets.gcode.colour.depth must be a whole number of layers, at least 1");
    }
    let filaments = get_or(col, "filaments", &empty_obj);
    let ok = filaments.as_object().is_some_and(|m| {
        m.values()
            .all(|v| is_int(v) && v.as_i64().is_some_and(|n| n >= 1))
    });
    if !ok {
        return err(
            "targets.gcode.colour.filaments must map tile groups to filament numbers (1, 2, ...)",
        );
    }
    let bottom = col.get("bottomDepth").cloned().unwrap_or(json!(1));
    if !is_int(&bottom) || bottom.as_i64().is_none_or(|d| d < 1) {
        return err(
            "targets.gcode.colour.bottomDepth must be a whole number of layers, at least 1",
        );
    }
    p["targets"][GCODE]["colour"] =
        json!({"mode": mode, "depth": depth, "bottomDepth": bottom, "filaments": filaments});
    let relief = object(gc, "relief", &empty_obj, "targets.gcode.relief")?;
    if let Some(k) = relief
        .keys()
        .find(|k| !["groutDepth", "raise"].contains(&k.as_str()))
    {
        return err(format!("unknown field targets.gcode.relief.{k}"));
    }
    let gd = relief.get("groutDepth").cloned().unwrap_or(json!(1));
    if !is_int(&gd) || !(1..=10).contains(&gd.as_i64().unwrap_or(0)) {
        return err(
            "targets.gcode.relief.groutDepth must be a whole number of layers, from 1 to 10",
        );
    }
    let raise = get_or(relief, "raise", &empty_obj);
    let ok = raise.as_object().is_some_and(|m| {
        m.values()
            .all(|v| is_int(v) && (0..=20).contains(&v.as_i64().unwrap_or(-1)))
    });
    if !ok {
        return err("targets.gcode.relief.raise must map tile groups to layers, from 0 to 20");
    }
    p["targets"][GCODE]["relief"] = json!({"groutDepth": gd, "raise": raise});
    let settings = get_or(gc, "settings", &empty_obj);
    let Some(settings) = settings.as_object() else {
        return err("targets.gcode.settings must be an object");
    };
    for (key, value) in settings {
        if PATTERN_SETTINGS.contains(&key.as_str()) {
            return err(format!(
                "{key} is a pattern setting: it goes in patternSettings, not targets.gcode.settings"
            ));
        }
        if !SETTINGS.iter().any(|(k, _)| k == key) {
            return err(format!("unknown setting '{key}'"));
        }
        check_setting(key, value, "targets.gcode.settings")?;
        p["targets"][GCODE]["settings"][key] = value.clone();
    }
    Ok(p)
}

/// A setting's value has its type (`at` is where it lives, for messages).
fn check_setting(key: &str, value: &Value, at: &str) -> Result<()> {
    let Some((_, kind)) = SETTINGS.iter().find(|(k, _)| *k == key) else {
        return err(format!("unknown setting '{key}'"));
    };
    if value.is_null() {
        return Ok(());
    }
    match kind {
        Kind::Bool if !value.is_boolean() => err(format!("{at}.{key} must be true or false")),
        Kind::Str if !value.is_string() => err(format!("{at}.{key} must be text")),
        Kind::Int | Kind::Float if !value.is_number() => {
            err(format!("{at}.{key} must be a number"))
        }
        // a count of loops (Python took 2.5 here, then failed printing it)
        Kind::Int if value.as_f64().is_some_and(|x| x.fract() != 0.0) => {
            err(format!("{at}.{key} must be a whole number"))
        }
        _ => Ok(()),
    }
}

fn placement(pl: &Value, at: &str) -> Result<Value> {
    let Some(m) = pl.as_object() else {
        return err(format!("{at} must be an object"));
    };
    let outline = match m.get("outline") {
        Some(Value::Array(a)) if a.len() >= 3 => a,
        _ => return err(format!("{at}.outline must list at least 3 points")),
    };
    let input = whole(m.get("input"), &format!("{at}.input"), 0)?;
    let plate = whole(m.get("plate"), &format!("{at}.plate"), 1)?;
    let piece = whole(m.get("piece"), &format!("{at}.piece"), 1)?;
    let mut pts = Vec::new();
    for q in outline {
        pts.push(pair(Some(q), &format!("{at}.outline"))?);
    }
    let rotation = num(
        Some(m.get("rotation").unwrap_or(&json!(0.0))),
        &format!("{at}.rotation"),
        false,
    )?;
    let position = pair(m.get("position"), &format!("{at}.position"))?;
    if position.is_null() {
        return err(format!("{at}.position is missing"));
    }
    Ok(
        json!({"input": input, "plate": plate, "piece": piece, "outline": pts,
              "rotation": rotation, "position": position}),
    )
}

/// Read and validate a project file; paths come back absolute.
/// A picture laid over the pattern (ADR 0009), as the pattern (ADR 0015): the
/// image placed once where the picture was, read as it was, at its width and
/// turn, its regions' line directions kept. The pattern under it is dropped.
fn from_picture(
    p: &mut Value,
    pic: &Map<String, Value>,
    base: Option<&Path>,
    lib: &Library,
) -> Result<()> {
    let file = file_path(pic.get("file"), "picture.file", base)?;
    let n = |k: &str| pic.get(k).and_then(Value::as_f64);
    p["patternPlacement"]["repeat"] = json!(false);
    p["patternPlacement"]
        .as_object_mut()
        .unwrap()
        .remove("align");
    let rotation = n("rotation").unwrap_or(0.0);
    p["patternPlacement"]["rotation"] = if rotation == 0.0 {
        Value::Null
    } else {
        json!(rotation)
    };
    p["patternPlacement"]["scale"] = json!(1.0);
    // the picture was centred on its origin (else the assembly's centre)
    let bounds = assembly::placements_bounds(&assembly::pattern_placements(p));
    p["patternPlacement"]["origin"] = match (pic.get("origin").and_then(Value::as_array), bounds) {
        (Some(o), Some(b)) if o.len() == 2 => json!([
            o[0].as_f64().unwrap_or(0.0) - b.center().x,
            o[1].as_f64().unwrap_or(0.0) - b.center().y
        ]),
        _ => Value::Null,
    };
    let width = n("width").unwrap_or_else(|| bounds.map_or(100.0, |b| b.width() / 2.0));
    let photo = pic.get("mode").and_then(Value::as_str) == Some("photo");
    let mut values = json!({"width": width, "gap": 0.0, "invert": pic.get("invert").and_then(Value::as_bool).unwrap_or(false)});
    if let Some(t) = n("threshold") {
        values["threshold"] = json!(t.round());
    }
    if patterns::is_bitmap(&file) {
        values["read"] = json!(if photo { "photo" } else { "logo" });
        if let Some(b) = n("bands") {
            values["bands"] = json!(b.round());
        }
    }
    p["pattern"] = patterns::check_ref(&json!({"image": file, "values": values}), lib)
        .map_err(|e| Error(format!("picture: {e}")))?;
    p["preset"] = Value::Null;
    // the regions' line directions were on the assembly; a pattern's turn with it
    let mut dirs = Map::new();
    if let Some(angles) = pic.get("angles").and_then(Value::as_array) {
        for (k, a) in angles.iter().enumerate() {
            let Some(a) = a.as_f64() else { continue };
            let group = if photo {
                patterns::BANDS.get(k).copied()
            } else {
                (k == 0).then_some(patterns::LOGO)
            };
            if let Some(g) = group {
                dirs.insert(g.into(), json!((a - rotation).rem_euclid(180.0)));
            }
        }
    }
    p["tileDirections"] = Value::Object(dirs);
    Ok(())
}

pub fn load(path: &Path, lib: &Library) -> Result<Value> {
    let data = std::fs::read_to_string(path)
        .map_err(|e| e.to_string())
        .and_then(|t| serde_json::from_str::<Value>(&t).map_err(|e| e.to_string()))
        .map_err(|e| Error(format!("can't read project {}: {e}", path.display())))?;
    let abs = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    validate(&data, abs.parent(), lib)
}

/// `os.path.relpath(v, base)` with "/" separators; None when it can't be made
/// relative (a file on another drive).
fn relpath(v: &Path, base: &Path) -> Option<String> {
    let v = normpath(v);
    let base = normpath(base);
    let a: Vec<Component> = v.components().collect();
    let b: Vec<Component> = base.components().collect();
    if a.first() != b.first() {
        return None;
    }
    let common = a.iter().zip(&b).take_while(|(x, y)| x == y).count();
    let mut parts: Vec<String> = vec!["..".into(); b.len() - common];
    parts.extend(
        a[common..]
            .iter()
            .map(|c| c.as_os_str().to_string_lossy().into_owned()),
    );
    Some(if parts.is_empty() {
        ".".into()
    } else {
        parts.join("/")
    })
}

/// Write a project, storing its paths relative to the project file.
pub fn save(project: &Value, path: &Path, lib: &Library) -> Result<()> {
    let mut p = validate(project, None, lib)?;
    let abs = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    let base = abs.parent().unwrap_or(Path::new("/")).to_path_buf();
    // Stored with "/" so a project opens on any system; a file on another
    // drive (Windows) can't be made relative and stays absolute.
    let rel = |v: &mut Value| {
        if let Some(s) = v
            .as_str()
            .filter(|s| !s.is_empty() && Path::new(s).is_absolute())
        {
            if let Some(r) = relpath(Path::new(s), &base) {
                *v = json!(r);
            }
        }
    };
    for i in p["source"]["inputs"].as_array_mut().unwrap() {
        rel(&mut i["path"]);
    }
    rel(&mut p["assembly"]["layoutSvg"]);
    for key in ["file", "image"] {
        if let Some(f) = p["pattern"].get_mut(key) {
            rel(f);
        }
    }
    let text = patterns::py_json_pretty(&p, 2, false) + "\n";
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    std::fs::write(&tmp, text).map_err(|e| Error(format!("can't write {}: {e}", tmp.display())))?;
    std::fs::rename(&tmp, path).map_err(|e| Error(format!("can't write {}: {e}", path.display())))
}

fn set_setting(args: &mut RunArgs, key: &str, v: &Value) {
    let f = v.as_f64();
    let i = || f.map(|x| x as i64);
    match key {
        "border" => args.border = i(),
        "edgeRim" => args.edge_rim = i(),
        "holeRim" => args.hole_rim = i(),
        "minArea" => args.min_area = f.unwrap_or(args.min_area),
        "flow" => args.flow = f.unwrap_or(args.flow),
        "printSpeed" => args.print_speed = f,
        "travelSpeed" => args.travel_speed = f.unwrap_or(args.travel_speed),
        "retract" => args.retract = f.unwrap_or(args.retract),
        "retractSpeed" => args.retract_speed = f.unwrap_or(args.retract_speed),
        "retractThreshold" => args.retract_threshold = f.unwrap_or(args.retract_threshold),
        "zhop" => args.zhop = f.unwrap_or(args.zhop),
        "close" => args.close = f.unwrap_or(args.close),
        "wallOverlap" => args.wall_overlap = f.unwrap_or(args.wall_overlap),
        "minCoverage" => args.min_coverage = f.unwrap_or(args.min_coverage),
        "lineWidth" => args.line_width = f.unwrap_or(args.line_width),
        "layerHeight" => args.layer_height = f.unwrap_or(args.layer_height),
        "noMerge" => args.no_merge = v.as_bool().unwrap_or(false),
        "svgDefaultAngle" => args.svg_default_angle = f,
        "svgPaths" => args.svg_paths = v.as_str().unwrap_or("direction").to_string(),
        "svgSkipUnmatched" => args.svg_skip_unmatched = v.as_bool().unwrap_or(false),
        _ => {}
    }
}

fn apply_settings(project: &Value, args: &mut RunArgs) {
    args.max_z = project["source"]["surfaces"]["maxZ"].as_f64();
    let settings = [&project["patternSettings"], &gcode(project)["settings"]];
    for (key, value) in settings.into_iter().filter_map(Value::as_object).flatten() {
        if !value.is_null() {
            set_setting(args, key, value);
        }
    }
}

/// Set run arguments from a (validated) project.
///
/// `plate` picks the plate's entry in assembly.assign (key "N" or
/// "INPUT:N"); a single-plate run from a slicer hook passes None.
pub fn apply_to_args(
    project: &Value,
    args: &mut RunArgs,
    plate: Option<&str>,
    lib: &Library,
) -> Result<()> {
    let asm = &project["assembly"];
    let r = &project["pattern"];
    let placed = asm["placements"].as_array().is_some_and(|a| !a.is_empty());
    if placed {
        // The pattern comes from the assembly; only the settings apply here,
        // with the same defaults as an SVG pattern.
        args.svg = None;
        args.pattern = None;
        args.pattern_svg = None;
        if !r.is_null() {
            let name = match r.get("file").or(r.get("image")).and_then(Value::as_str) {
                Some(f) => Path::new(f)
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default(),
                None => r["id"].as_str().unwrap_or("").to_string(),
            };
            args.pattern_label = Some(format!("assembly + pattern {name}"));
        }
        args.border.get_or_insert(0);
        args.tile_gap.get_or_insert(0.0);
        apply_settings(project, args);
        return Ok(());
    }
    let layout = asm["layoutSvg"].as_str();
    if !r.is_null() && layout.is_none() {
        return err("the project has a pattern but no assembly (assembly.layoutSvg)");
    }
    if project["tileDirections"]
        .as_object()
        .is_some_and(|m| !m.is_empty())
    {
        return err("changed line directions need an assembly of placed pieces, not a layout SVG");
    }
    if let Some(l) = layout {
        args.svg = Some(l.to_string());
    }
    if !r.is_null() {
        match r.get("file").and_then(Value::as_str) {
            Some(f) => args.pattern = Some(f.to_string()),
            None => {
                args.pattern_svg =
                    Some(patterns::resolve(Some(r), lib, None).map_err(|e| Error(e.0))?)
            }
        }
        let pp = &project["patternPlacement"];
        args.pattern_rotate = pp["rotation"].as_f64();
        args.pattern_origin = pp["origin"]
            .as_array()
            .map(|o| (o[0].as_f64().unwrap_or(0.0), o[1].as_f64().unwrap_or(0.0)));
        args.pattern_scale = pp["scale"].as_f64().unwrap_or(1.0);
        args.pattern_grout = project["grout"].as_f64();
    }
    if let Some(a) = plate
        .and_then(|k| asm["assign"].get(k))
        .and_then(Value::as_str)
    {
        args.svg_assign = Some(a.to_string());
    }
    apply_settings(project, args);
    Ok(())
}

/// Values for unset project settings, from the slicer's own settings (first extruder).
pub fn slicer_defaults(cfg: &SlicerConfig) -> Map<String, Value> {
    let mut out = Map::new();
    for (key, names) in SLICER_KEYS {
        for name in names {
            let v = match cfg.get(name) {
                None => continue,
                Some(SV::Float(x)) => Some(*x),
                Some(SV::Int(x)) => Some(*x as f64),
                Some(SV::Floats(v)) => v.first().copied().flatten(),
                Some(SV::Ints(v)) => v.first().copied().flatten().map(|x| x as f64),
                Some(_) => None,
            };
            if let Some(x) = v {
                out.insert(key.to_string(), json!(x));
                break;
            }
        }
    }
    out
}

/// Use the slicer's retraction, z-hop and travel speed where the project doesn't set them.
pub fn apply_slicer_defaults(project: &Value, args: &mut RunArgs, cfg: &SlicerConfig) {
    for (key, value) in slicer_defaults(cfg) {
        if gcode(project)["settings"]
            .get(&key)
            .is_none_or(Value::is_null)
        {
            set_setting(args, &key, &value);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lib() -> Library {
        Library::new(Some(std::env::temp_dir().join("pp-project-test-none")))
    }

    #[test]
    fn new_project_validates_to_itself() {
        assert_eq!(validate(&new(), None, &lib()).unwrap(), new());
    }

    #[test]
    fn a_pattern_placed_once_keeps_where_it_lines_up() {
        let pp = |v: Value| {
            let p = json!({"schemaVersion": 3, "patternPlacement": v});
            validate(&p, None, &lib()).map(|p| p["patternPlacement"].clone())
        };
        // repeating and centred are left out
        let v = pp(json!({"repeat": true, "align": "center"})).unwrap();
        assert!(v.get("repeat").is_none() && v.get("align").is_none(), "{v}");
        let v = pp(json!({"repeat": false, "align": "top-left"})).unwrap();
        assert_eq!(
            (&v["repeat"], &v["align"]),
            (&json!(false), &json!("top-left"))
        );
        assert!(pp(json!({"align": "middle"})).is_err());
        assert!(pp(json!({"repeat": "no"})).is_err());
    }

    #[test]
    fn an_image_pattern_names_its_file_and_reads_by_its_form() {
        let p =
            json!({"schemaVersion": 3, "pattern": {"image": "logo.png", "values": {"width": 50}}});
        let v = validate(&p, Some(Path::new("/x")), &lib()).unwrap();
        let want = path_str(&normpath(Path::new("/x/logo.png")));
        assert_eq!(v["pattern"]["image"], want.as_str());
        // the form's defaults fill in the rest; a bitmap can be read as a photo
        assert_eq!(v["pattern"]["values"]["gap"], 10.0);
        assert_eq!(v["pattern"]["values"]["read"], "logo");
        let bad =
            json!({"schemaVersion": 3, "pattern": {"image": "logo.svg", "values": {"width": 0}}});
        assert!(validate(&bad, None, &lib()).is_err());
    }

    #[test]
    fn paths_resolve_against_the_project_folder() {
        let p = json!({"schemaVersion": 3, "source": {"inputs": [{"path": "a/../b.gcode"}]}});
        let v = validate(&p, Some(Path::new("/x/y")), &lib()).unwrap();
        // in the platform's own form (backslashes on Windows)
        let want = path_str(&normpath(Path::new("/x/y/b.gcode")));
        assert_eq!(v["source"]["inputs"][0]["path"], want.as_str());
        assert_eq!(v["source"]["kind"], "sliced");
        assert_eq!(
            relpath(Path::new("/x/y/b.gcode"), Path::new("/x/z")).unwrap(),
            "../y/b.gcode"
        );
    }

    #[test]
    fn rejects_bad_fields() {
        let bad = |p: Value| validate(&p, None, &lib()).unwrap_err().0;
        assert_eq!(
            bad(json!({"schemaVersion": 4})),
            "unsupported schemaVersion 4 (this engine reads 1 to 3)"
        );
        assert_eq!(
            bad(json!({"schemaVersion": 3, "zz": 1, "aa": 2})),
            "unknown field(s): aa, zz"
        );
        assert_eq!(
            bad(json!({"schemaVersion": 3, "targets": {"gcode": {"colour": {"depth": 0}}}})),
            "targets.gcode.colour.depth must be a whole number of layers, at least 1"
        );
        assert_eq!(
            bad(json!({"schemaVersion": 3, "targets": {"gcode": {"settings": {"flow": true}}}})),
            "targets.gcode.settings.flow must be a number"
        );
        assert_eq!(
            bad(
                json!({"schemaVersion": 3, "targets": {"gcode": {"settings": {"svgPaths": "literal"}}}})
            ),
            "svgPaths is a pattern setting: it goes in patternSettings, not targets.gcode.settings"
        );
        assert_eq!(
            bad(json!({"schemaVersion": 3, "patternSettings": {"flow": 1.0}})),
            "unknown pattern setting 'flow'"
        );
        assert_eq!(
            bad(json!({"schemaVersion": 3, "source": {"surfaces": {"faces": ["side"]}}})),
            "source.surfaces.faces must list \"top\", \"bottom\" or both"
        );
        assert_eq!(
            bad(json!({"schemaVersion": 3, "source": {"kind": "vector"}})),
            "this project's pieces come from a source this version of TessaFab can't read (vector)"
        );
        assert_eq!(
            bad(
                json!({"schemaVersion": 3, "assembly": {"placements": [{"outline": [[0,0],[1,0],[1,1]], "input": 0, "plate": 1, "piece": 1}]}})
            ),
            "assembly.placements[0].position is missing"
        );
    }

    #[test]
    fn version_2_moves_into_source_and_targets() {
        let v2 = json!({"schemaVersion": 2, "inputs": [{"path": "/a.gcode"}],
            "pattern": {"id": "herringbone"},
            "surfaces": {"maxZ": 4.0, "excluded": ["0:1:5@1.000"]},
            "colour": {"mode": "ams", "depth": 3, "filaments": {"#ff0000": 2}},
            "settings": {"edgeRim": 2, "svgPaths": "literal", "svgDefaultAngle": 30.0}});
        let v = validate(&v2, None, &lib()).unwrap();
        assert_eq!(v["schemaVersion"], 3);
        assert_eq!(
            v["source"],
            json!({"kind": "sliced", "inputs": [{"path": "/a.gcode"}],
                   "surfaces": {"faces": ["top"], "maxZ": 4.0, "excluded": ["0:1:5@1.000"]}})
        );
        assert_eq!(
            v["targets"],
            json!({"gcode": {"colour": {"mode": "ams", "depth": 3, "bottomDepth": 1, "filaments": {"#ff0000": 2}},
                             "settings": {"edgeRim": 2}, "relief": {"groutDepth": 1, "raise": {}}}})
        );
        assert_eq!(
            v["patternSettings"],
            json!({"svgPaths": "literal", "svgDefaultAngle": 30.0})
        );
        for k in ["inputs", "surfaces", "colour", "settings"] {
            assert!(v.get(k).is_none(), "{k} left at the top");
        }
        // checked where the field now lives
        let bad = json!({"schemaVersion": 2, "colour": {"depth": 0}});
        assert_eq!(
            validate(&bad, None, &lib()).unwrap_err().0,
            "targets.gcode.colour.depth must be a whole number of layers, at least 1"
        );
    }

    #[test]
    fn version_3_validates_to_itself_and_keeps_unknown_targets() {
        let mut p = new();
        p["source"]["inputs"] = json!([{"path": "/a.gcode"}]);
        p["targets"]["svg"] = json!({"lineSpacing": 0.4, "layers": ["#ff0000"]});
        p["patternSettings"]["svgPaths"] = json!("literal");
        p["targets"]["gcode"]["settings"]["border"] = json!(1);
        let v = validate(&p, None, &lib()).unwrap();
        assert_eq!(v, p);
        assert_eq!(validate(&v, None, &lib()).unwrap(), v);
    }

    #[test]
    fn version_1_generator_becomes_a_pattern() {
        let p = json!({"schemaVersion": 1, "template": {"generator": {"family": "herringbone_strip", "params": {}}}});
        let v = validate(&p, None, &lib()).unwrap();
        assert_eq!(v["schemaVersion"], 3);
        assert_eq!(v["pattern"]["id"], "herringbone");
        assert_eq!(v["pattern"]["values"]["layout"], "diagonal");
        assert_eq!(v["source"]["kind"], "sliced");
    }

    #[test]
    fn settings_apply_to_args() {
        let p = validate(&json!({"schemaVersion": 3, "pattern": {"id": "herringbone"},
            "assembly": {"placements": [{"outline": [[0,0],[1,0],[1,1]], "input": 0, "plate": 1, "piece": 1, "position": [0, 0]}]},
            "source": {"surfaces": {"maxZ": 7.5}},
            "patternSettings": {"svgDefaultAngle": 15.0},
            "targets": {"gcode": {"settings": {"edgeRim": 3, "flow": 0.9}}}}), None, &lib()).unwrap();
        let mut a = RunArgs::default();
        apply_to_args(&p, &mut a, None, &lib()).unwrap();
        assert_eq!(
            (a.edge_rim, a.flow, a.border, a.tile_gap),
            (Some(3), 0.9, Some(0), Some(0.0))
        );
        assert_eq!((a.max_z, a.svg_default_angle), (Some(7.5), Some(15.0)));
        assert_eq!(
            a.pattern_label.as_deref(),
            Some("assembly + pattern herringbone")
        );
    }
}
