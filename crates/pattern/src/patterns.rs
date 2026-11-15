//! Patterns, presets and the user's pattern library. A port of patterns.py.
//!
//! A pattern is a tile design with parameters (a Form.io form). Provided
//! patterns are built in code (`library`); user patterns are SVG files in the
//! library whose numbers may be `{{ expressions }}` over the parameters
//! (`expr`), with the form and tile-group names in a `<metadata>` element.
//!
//! A preset is saved parameter values (and line directions) for one pattern.
//! Every pattern has a "default" preset (its form's defaults); provided
//! patterns may have more provided presets; the user's own presets are kept in
//! the library's presets.json.
//!
//! A project refers to its pattern as `{"id": ..., "values": {...}}` (a
//! provided id, or "user:<name>" for a library pattern), `{"file": path}`
//! for a plain pattern SVG used as drawn, or `{"image": path, "values": {...}}`
//! for an image file read as a pattern (ADR 0015).
//!
//! An image pattern (a library one, marked `"image": true` in its metadata
//! and holding the image as a data URL, or an image file) has the image's
//! reading as its parameters; its page, a drawing like any pattern's, comes
//! from the image reader the Pro build registers ([`set_image_reader`]).

use crate::direction::Direction;
use crate::drawing::TileDrawing;
use crate::expr::{self, Values};
use crate::form;
use crate::library::{self, py_json};
use crate::svg;
use serde_json::{json, Map, Value};
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

pub const USER: &str = "user:";
/// A pattern from an installed pack (ADR 0011): `pack:<pack id>/<name>`.
pub const PACK: &str = "pack:";

/// A pack pattern id's parts: (pack id, pattern name).
pub fn pack_parts(id: &str) -> Option<(&str, &str)> {
    let (p, n) = id.strip_prefix(PACK)?.split_once('/')?;
    (name_ok(p) && name_ok(n)).then_some((p, n))
}

/// Whether a pattern name or pack id is safe as a file name (letters, digits, `_`, `-`).
pub fn safe_name(name: &str) -> bool {
    name_ok(name)
}
pub const META_ID: &str = "tessafab";

// --------------------------------------------------------------------------- //
// Image patterns (ADR 0015)
// --------------------------------------------------------------------------- //

/// An image a pattern is read from: a file, or the data URL a library image
/// pattern keeps.
pub enum Image<'a> {
    File(&'a Path),
    Data(&'a str),
}

/// Reads images into pattern pages. The Pro build registers one.
pub trait ImageReader: Send + Sync {
    /// The image's page at `values` (its image form's): a drawing SVG, one
    /// repeat, the image's regions as tiles on a background tile.
    fn page(
        &self,
        image: Image<'_>,
        values: &Map<String, Value>,
    ) -> std::result::Result<String, String>;
    /// An image file as a data URL, and its height over its width.
    fn data_url(&self, path: &Path) -> std::result::Result<(String, f64), String>;
}

static IMAGE_READER: OnceLock<Box<dyn ImageReader>> = OnceLock::new();

/// Register the image reader (once; later calls are ignored).
pub fn set_image_reader(r: Box<dyn ImageReader>) {
    let _ = IMAGE_READER.set(r);
}

fn image_reader() -> Result<&'static dyn ImageReader> {
    IMAGE_READER.get().map(|b| b.as_ref()).ok_or_else(|| {
        Error("image patterns are part of TessaFab Pro: this build can't read images".into())
    })
}

/// The tile group of everything round an image, and beyond a pattern placed once.
pub const BACKGROUND: &str = "#eeeeee";
/// The tile group of a logo.
pub const LOGO: &str = "#202830";
/// The tile groups of a photo's brightness bands, darkest first (a photo of
/// n bands takes the first n).
pub const BANDS: [&str; 8] = [
    "#1a1a1a", "#333333", "#4d4d4d", "#666666", "#808080", "#999999", "#b3b3b3", "#cccccc",
];
/// The files an image pattern can be read from.
pub const IMAGE_EXTS: [&str; 4] = ["svg", "png", "jpg", "jpeg"];

/// Whether `path` is a bitmap (a photo can be read from it), not an SVG.
pub fn is_bitmap(path: &str) -> bool {
    !path.to_lowercase().ends_with(".svg")
}

/// An image pattern's parameters: how wide, the gap round it, and how it is
/// read (a bitmap can be read as a photo).
pub fn image_form(bitmap: bool) -> Value {
    let number = |key: &str,
                  label: &str,
                  default: f64,
                  lo: f64,
                  hi: f64,
                  unit: &str,
                  integer: bool| {
        let mut c = json!({"type": "number", "key": key, "label": label, "input": true, "defaultValue": default,
            "validate": {"required": true, "min": lo, "max": hi, "integer": integer},
            "decimalLimit": if integer { 0 } else { 2 }, "requireDecimal": false, "delimiter": false,
            "properties": {"step": "1"}});
        if !unit.is_empty() {
            c["suffix"] = json!(unit);
        }
        c
    };
    let mut cs = vec![number("width", "Width", 80.0, 1.0, 2000.0, "mm", false), {
        let mut g = number("gap", "Gap", 10.0, 0.0, 500.0, "mm", false);
        g["description"] = json!("Space round the image where it repeats.");
        g
    }];
    let logo_only = json!({"show": true, "when": "read", "eq": "logo"});
    if bitmap {
        cs.push(json!({"type": "radio", "key": "read", "label": "Read as", "input": true, "inline": true,
            "defaultValue": "logo", "values": [{"label": "Logo", "value": "logo"}, {"label": "Photo", "value": "photo"}]}));
    }
    let mut threshold = number("threshold", "Threshold", 128.0, 0.0, 255.0, "", true);
    threshold["description"] = json!("Darker than this is the logo.");
    if bitmap {
        threshold["conditional"] = logo_only;
    }
    cs.push(threshold);
    if bitmap {
        let mut bands = number("bands", "Bands", 4.0, 2.0, 8.0, "", true);
        bands["description"] = json!("Brightness bands, each with its own line direction.");
        bands["conditional"] = json!({"show": true, "when": "read", "eq": "photo"});
        cs.push(bands);
    }
    cs.push(json!({"type": "checkbox", "key": "invert", "label": "Invert", "input": true, "defaultValue": false}));
    json!({"components": cs})
}

/// The tile groups' names of an image pattern.
pub fn image_groups() -> Map<String, Value> {
    let mut m = Map::new();
    m.insert(LOGO.into(), json!("Logo"));
    for (k, c) in BANDS.iter().enumerate() {
        let name = match k {
            0 => "Band 1 (darkest)".to_string(),
            k => format!("Band {}", k + 1),
        };
        m.insert((*c).into(), json!(name));
    }
    m.insert(BACKGROUND.into(), json!("Background"));
    m
}

/// The data URL of a library image pattern's image.
fn image_href(text: &str) -> Result<String> {
    let opts = roxmltree::ParsingOptions {
        allow_dtd: true,
        ..Default::default()
    };
    let doc = roxmltree::Document::parse_with_options(text, opts)
        .map_err(|e| Error(format!("the pattern SVG can't be read: {e}")))?;
    doc.descendants()
        .filter(|n| n.is_element() && n.tag_name().name() == "image")
        .find_map(|n| {
            n.attribute("href")
                .or_else(|| n.attribute(("http://www.w3.org/1999/xlink", "href")))
        })
        .filter(|h| h.starts_with("data:"))
        .map(String::from)
        .ok_or_else(|| Error("the image pattern has no image".into()))
}

/// A pattern file at `values`: an image pattern's page, else the drawing with
/// its expressions worked out.
pub fn render_pattern(text: &str, d: &Definition, values: &Map<String, Value>) -> Result<String> {
    if d.image {
        let href = image_href(text)?;
        return image_reader()?
            .page(Image::Data(&href), values)
            .map_err(Error);
    }
    render_user_svg(text, values)
}

#[derive(Debug, Clone, PartialEq)]
pub struct Error(pub String);

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Error {}

impl From<library::Error> for Error {
    fn from(e: library::Error) -> Self {
        Error(e.0)
    }
}

impl From<form::Error> for Error {
    fn from(e: form::Error) -> Self {
        Error(e.0)
    }
}

impl From<svg::Error> for Error {
    fn from(e: svg::Error) -> Self {
        Error(e.0)
    }
}

fn err<T>(m: impl Into<String>) -> Result<T> {
    Err(Error(m.into()))
}

pub type Result<T> = std::result::Result<T, Error>;

/// The app's own folder for the user (Application Support, AppData, .config):
/// `TessaFab`, or `Pattern Print` (the app's old name) while only that exists.
pub fn app_data_dir() -> PathBuf {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default();
    let base = if cfg!(target_os = "macos") {
        home.join("Library/Application Support")
    } else if cfg!(windows) {
        std::env::var_os("APPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join("AppData/Roaming"))
    } else {
        std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".config"))
    };
    let now = base.join("TessaFab");
    let old = base.join("Pattern Print");
    if !now.exists() && old.exists() {
        old
    } else {
        now
    }
}

/// Where the desktop app keeps the user's patterns and presets (`$TESSAFAB_LIBRARY` if set).
pub fn default_library_dir() -> PathBuf {
    if let Some(d) = std::env::var_os("TESSAFAB_LIBRARY") {
        return PathBuf::from(d);
    }
    app_data_dir().join("patterns")
}

fn name_ok(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-')
}

/// Python's `round(x, n)` for the few places groups' directions are rounded.
fn round_to(x: f64, n: usize) -> f64 {
    format!("{x:.n$}").parse().unwrap_or(x)
}

fn values_of(m: &Map<String, Value>) -> Values {
    m.iter()
        .filter_map(|(k, v)| expr::Value::from_json(v).map(|v| (k.clone(), v)))
        .collect()
}

// --------------------------------------------------------------------------- //
// User pattern files
// --------------------------------------------------------------------------- //

/// A user pattern's definition: name, form, tile group labels.
#[derive(Debug, Clone)]
pub struct Definition {
    pub name: String,
    pub form: Value,
    /// colour -> label, in file order.
    pub groups: Map<String, Value>,
    /// An image pattern (ADR 0015).
    pub image: bool,
}

/// (source text, definition) of a user pattern file.
pub fn read_user_svg(path: &Path) -> Result<(String, Definition)> {
    let text = fs::read_to_string(path)
        .map_err(|e| Error(format!("can't read {}: {}", path.display(), e)))?;
    let fallback = path
        .file_stem()
        .map(|s| s.to_string_lossy().replace('_', " "))
        .unwrap_or_default();
    let d = definition_of(&text, &fallback)?;
    Ok((text, d))
}

pub fn definition_of(text: &str, fallback_name: &str) -> Result<Definition> {
    let opts = roxmltree::ParsingOptions {
        allow_dtd: true,
        ..Default::default()
    };
    let doc = roxmltree::Document::parse_with_options(text, opts)
        .map_err(|e| Error(format!("the pattern SVG can't be read: {e}")))?;
    let (mut meta, mut title): (Option<Value>, Option<String>) = (None, None);
    for el in doc.descendants().filter(|n| n.is_element()) {
        let tag = el.tag_name().name();
        let text: String = el.children().filter_map(|c| c.text()).collect();
        if tag == "metadata" && el.attribute("id") == Some(META_ID) && !text.is_empty() {
            meta = Some(
                serde_json::from_str(&text)
                    .map_err(|_| Error("the pattern's metadata isn't valid JSON".into()))?,
            );
        }
        if tag == "title" && !text.is_empty() && title.is_none() {
            title = Some(text.trim().to_string());
        }
    }
    let meta = meta.unwrap_or(json!({}));
    let form = match meta.get("form") {
        Some(f) if !f.is_null() && f != &json!({}) => f.clone(),
        _ => json!({"components": []}),
    };
    form::validate_form(&form)?;
    let name = meta
        .get("name")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(String::from)
        .or(title)
        .unwrap_or_else(|| fallback_name.to_string());
    let groups = meta
        .get("groups")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let image = meta.get("image").and_then(Value::as_bool).unwrap_or(false);
    Ok(Definition {
        name,
        form,
        groups,
        image,
    })
}

/// The pattern SVG at `values`: every `{{ expression }}` replaced by its value.
pub fn render_user_svg(text: &str, values: &Map<String, Value>) -> Result<String> {
    expr::substitute(text, &values_of(values)).map_err(|e| Error(e.0))
}

fn html_escape_noquote(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// `text` with its `<metadata id="tessafab">` replaced by `definition`.
pub fn with_metadata(text: &str, d: &Definition) -> Result<String> {
    let mut meta_json = json!({"name": d.name, "form": d.form, "groups": d.groups});
    if d.image {
        meta_json["image"] = json!(true);
    }
    let meta = format!(
        "<metadata id=\"{META_ID}\">{}</metadata>",
        html_escape_noquote(&py_json(&meta_json))
    );
    // drop any metadata block of ours, with the white space before it
    let open = format!("<metadata id=\"{META_ID}\">");
    let mut t = text.to_string();
    while let Some(a) = t.find(&open) {
        let Some(b) = t[a..].find("</metadata>") else {
            break;
        };
        let end = a + b + "</metadata>".len();
        let start = t[..a].trim_end_matches(|c: char| c.is_whitespace()).len();
        t.replace_range(start..end, "");
    }
    // after the opening <svg ...> tag
    let Some(s) = find_svg_open(&t) else {
        return err("the pattern isn't an SVG (no <svg> element)");
    };
    Ok(format!("{}\n{}{}", &t[..s], meta, &t[s..]))
}

/// End of the first `<svg ...>` start tag (`<svg\b[^>]*>`).
fn find_svg_open(t: &str) -> Option<usize> {
    let mut from = 0;
    while let Some(a) = t[from..].find("<svg") {
        let a = from + a;
        let next = t[a + 4..].chars().next();
        if next.is_none_or(|c| !(c.is_alphanumeric() || c == '_')) {
            return t[a..].find('>').map(|b| a + b + 1);
        }
        from = a + 4;
    }
    None
}

fn tiles_of(text: &str) -> Result<TileDrawing> {
    let d = svg::read_svg(text, None, 0.15)?;
    Ok(TileDrawing::from_drawing(&d, None, false)?)
}

/// An error unless `text` (a pattern SVG at some values) can be used as a pattern.
pub fn check_drawing(text: &str) -> Result<()> {
    let d = tiles_of(text)?;
    if d.tiles.is_empty() {
        return err("the drawing has no tiles (filled shapes)");
    }
    Ok(())
}

fn dir_json(t: &crate::drawing::Tile) -> Value {
    match t.direction {
        Direction::Concentric => json!("rings"),
        Direction::Angle(a) => json!(round_to(a, 3)),
    }
}

// --------------------------------------------------------------------------- //
// The library
// --------------------------------------------------------------------------- //

/// The user's patterns (`<dir>/<name>.svg`) and presets (`<dir>/presets.json`).
pub struct Library {
    pub dir: PathBuf,
}

impl Library {
    pub fn new(dir: Option<PathBuf>) -> Self {
        Library {
            dir: dir.unwrap_or_else(default_library_dir),
        }
    }

    pub fn path_of(&self, id: &str) -> Result<PathBuf> {
        if let Some((pack, name)) = pack_parts(id) {
            return Ok(self
                .packs_dir()
                .join(pack)
                .join("patterns")
                .join(format!("{name}.svg")));
        }
        let Some(name) = id.strip_prefix(USER) else {
            return err(format!("{id} isn't one of your patterns"));
        };
        if !name_ok(name) {
            return err(format!("bad pattern id '{id}'"));
        }
        Ok(self.dir.join(format!("{name}.svg")))
    }

    /// Installed packs: one folder each (`packs/<id>/pack.json`, `patterns/`, `presets.json`).
    pub fn packs_dir(&self) -> PathBuf {
        self.dir.join("packs")
    }

    /// An installed pack's `pack.json`.
    pub fn pack_meta(&self, pack: &str) -> Option<Value> {
        let text = fs::read_to_string(self.packs_dir().join(pack).join("pack.json")).ok()?;
        serde_json::from_str(&text).ok()
    }

    /// Installed packs' ids, sorted.
    pub fn pack_list(&self) -> Vec<String> {
        let Ok(rd) = fs::read_dir(self.packs_dir()) else {
            return vec![];
        };
        let mut ids: Vec<String> = rd
            .filter_map(|e| e.ok())
            .filter(|e| e.path().join("pack.json").exists())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| name_ok(n))
            .collect();
        ids.sort();
        ids
    }

    /// Every installed pack pattern's id.
    pub fn pack_ids(&self) -> Vec<String> {
        let mut out = Vec::new();
        for pack in self.pack_list() {
            let Ok(rd) = fs::read_dir(self.packs_dir().join(&pack).join("patterns")) else {
                continue;
            };
            let mut names: Vec<String> = rd
                .filter_map(|e| e.ok())
                .filter_map(|e| {
                    let f = e.file_name().to_string_lossy().into_owned();
                    f.strip_suffix(".svg").map(String::from)
                })
                .filter(|n| name_ok(n))
                .collect();
            names.sort();
            out.extend(names.into_iter().map(|n| format!("{PACK}{pack}/{n}")));
        }
        out
    }

    /// The user's own presets of a pattern (for a pack, with the pattern).
    pub fn own_presets(&self, id: &str) -> Vec<Value> {
        self.read_presets()
            .get(id)
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
    }

    /// A pack's own presets of one of its patterns (`presets.json`: `{"patterns": {name: [...]}}`).
    fn pack_presets(&self, id: &str) -> Vec<Value> {
        let Some((pack, name)) = pack_parts(id) else {
            return vec![];
        };
        let text = fs::read_to_string(self.packs_dir().join(pack).join("presets.json"))
            .unwrap_or_default();
        serde_json::from_str::<Value>(&text)
            .ok()
            .and_then(|v| v["patterns"][name].as_array().cloned())
            .unwrap_or_default()
            .into_iter()
            .map(|mut p| {
                p["provided"] = json!(true);
                p
            })
            .collect()
    }

    fn presets_file(&self) -> PathBuf {
        self.dir.join("presets.json")
    }

    fn read_presets(&self) -> Map<String, Value> {
        let Ok(text) = fs::read_to_string(self.presets_file()) else {
            return Map::new();
        };
        match serde_json::from_str::<Value>(&text) {
            Ok(Value::Object(m)) => m
                .get("patterns")
                .and_then(Value::as_object)
                .cloned()
                .unwrap_or_default(),
            _ => Map::new(),
        }
    }

    fn write_presets(&self, presets: &Map<String, Value>) -> Result<()> {
        fs::create_dir_all(&self.dir).map_err(|e| Error(e.to_string()))?;
        let text = py_json_indent(&json!({"version": 1, "patterns": presets}), 2);
        let tmp = self.presets_file().with_extension("json.tmp");
        fs::write(&tmp, text)
            .and_then(|_| fs::rename(&tmp, self.presets_file()))
            .map_err(|e| Error(e.to_string()))
    }

    pub fn user_ids(&self) -> Vec<String> {
        let Ok(rd) = fs::read_dir(&self.dir) else {
            return vec![];
        };
        let mut names: Vec<String> = rd
            .filter_map(|e| e.ok())
            .filter_map(|e| {
                let f = e.file_name().to_string_lossy().into_owned();
                f.to_lowercase()
                    .ends_with(".svg")
                    .then(|| f[..f.len() - 4].to_string())
            })
            .collect();
        names.sort();
        names
            .into_iter()
            .filter(|n| name_ok(n))
            .map(|n| format!("{USER}{n}"))
            .collect()
    }

    /// `{"id", "name", "provided", "form", "groups": [{group, label, direction}], "source"?}`.
    pub fn definition(&self, id: &str) -> Result<Value> {
        if library::spec(id).is_ok() {
            let p = library::provided()?
                .into_iter()
                .find(|x| x["id"] == id)
                .unwrap();
            return Ok(
                json!({"id": p["id"], "name": p["name"], "provided": p["provided"], "form": p["form"], "groups": p["groups"]}),
            );
        }
        let path = self.path_of(id)?;
        if !path.exists() {
            return err(format!("pattern not found: {}", &id[USER.len()..]));
        }
        let (text, d) = read_user_svg(&path)?;
        let pack = pack_parts(id).and_then(|(p, _)| self.pack_meta(p));
        let mut groups = Vec::new();
        if let Ok(rendered) = render_pattern(&text, &d, &form::defaults(&d.form)) {
            if let Ok(drawing) = tiles_of(&rendered) {
                let mut seen: Vec<(String, Value)> = Vec::new();
                for t in &drawing.tiles {
                    if !seen.iter().any(|(c, _)| *c == t.color) {
                        seen.push((t.color.clone(), dir_json(t)));
                    }
                }
                groups = seen
                    .into_iter()
                    .enumerate()
                    .map(|(k, (c, v))| {
                        let label = d
                            .groups
                            .get(&c)
                            .and_then(Value::as_str)
                            .filter(|s| !s.is_empty())
                            .map(String::from)
                            .unwrap_or_else(|| format!("Group {}", k + 1));
                        json!({"group": c, "label": label, "direction": v})
                    })
                    .collect();
            }
        }
        let mut out = json!({"id": id, "name": d.name, "provided": false, "form": d.form, "groups": groups, "source": text});
        if d.image {
            out["image"] = json!(true);
            // the image pattern's source is the image, not a drawing to edit
            out.as_object_mut().unwrap().remove("source");
        }
        if let Some(meta) = pack {
            out["pack"] = meta;
            out["readOnly"] = json!(true);
        }
        Ok(out)
    }

    fn user_presets(all: &Map<String, Value>, id: &str) -> Vec<Value> {
        all.get(id)
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .map(|p| {
                        let mut p = p.clone();
                        p["provided"] = json!(false);
                        p
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Every pattern (provided first) with its presets: default, provided, then the user's.
    pub fn list(&self) -> Result<Vec<Value>> {
        let user = self.read_presets();
        let mut out = Vec::new();
        for mut p in library::provided()? {
            let id = p["id"].as_str().unwrap().to_string();
            let mut presets = p["presets"].as_array().unwrap().clone();
            presets.extend(Self::user_presets(&user, &id));
            p["presets"] = Value::Array(presets);
            out.push(p);
        }
        for id in self.user_ids().into_iter().chain(self.pack_ids()) {
            match self.definition(&id) {
                Err(e) => out.push(json!({"id": id, "name": &id[USER.len()..], "provided": false, "parametric": false,
                    "form": {"components": []}, "groups": [], "presets": [], "error": e.0})),
                Ok(mut d) => {
                    let defaults = form::defaults(&d["form"]);
                    d.as_object_mut().unwrap().remove("source");
                    d["parametric"] = json!(!form::inputs(&d["form"]).is_empty());
                    let mut presets = vec![json!({"id": "default", "name": "Default", "provided": true, "values": defaults, "directions": {}})];
                    presets.extend(self.pack_presets(&id));
                    presets.extend(Self::user_presets(&user, &id));
                    d["presets"] = Value::Array(presets);
                    out.push(d);
                }
            }
        }
        Ok(out)
    }

    /// Create (no id) or replace a user pattern. Checks it renders at its defaults.
    pub fn save_pattern(
        &self,
        name: &str,
        source: &str,
        form_v: &Value,
        groups: Option<&Map<String, Value>>,
        id: Option<&str>,
    ) -> Result<Value> {
        if id.is_some_and(|i| i.starts_with(PACK)) {
            return err("a pack's patterns can't be changed; make your own from it");
        }
        let name = name.trim();
        let name = if name.is_empty() { "My pattern" } else { name };
        form::validate_form(form_v)?;
        let d = Definition {
            name: name.into(),
            form: form_v.clone(),
            groups: groups.cloned().unwrap_or_default(),
            image: false,
        };
        let text = with_metadata(source, &d)?;
        let params: BTreeSet<String> = form::defaults(form_v).keys().cloned().collect();
        let unknown: Vec<String> = expr::names_used(&text)
            .into_iter()
            .filter(|n| !params.contains(n))
            .collect();
        if !unknown.is_empty() {
            return err(format!(
                "the drawing uses {}, which {} not a parameter",
                unknown.join(", "),
                if unknown.len() == 1 { "is" } else { "are" }
            ));
        }
        check_drawing(&render_user_svg(&text, &form::defaults(form_v))?)?;
        let id = match id {
            Some(i) => i.to_string(),
            None => self.new_id(name),
        };
        let path = self.path_of(&id)?;
        fs::create_dir_all(&self.dir)
            .and_then(|_| fs::write(&path, text))
            .map_err(|e| Error(e.to_string()))?;
        Ok(json!({"id": id}))
    }

    fn new_id(&self, name: &str) -> String {
        let mut stem = String::new();
        let mut gap = false;
        for c in name.chars() {
            if c.is_ascii_alphanumeric() {
                if gap && !stem.is_empty() {
                    stem.push('_');
                }
                gap = false;
                stem.push(c.to_ascii_lowercase());
            } else {
                gap = true;
            }
        }
        // Python: re.sub runs of non-alphanumerics to "_" then strip("_")
        if stem.is_empty() {
            stem = "pattern".into();
        }
        let taken: BTreeSet<String> = self.user_ids().into_iter().collect();
        let mut k = 1;
        let mut id = format!("{USER}{stem}");
        while taken.contains(&id) {
            k += 1;
            id = format!("{USER}{stem}_{k}");
        }
        id
    }

    pub fn delete_pattern(&self, id: &str) -> Result<Value> {
        if id.starts_with(PACK) {
            return err("a pack's patterns can't be deleted one by one; remove the pack");
        }
        let path = self.path_of(id)?;
        if path.exists() {
            fs::remove_file(&path).map_err(|e| Error(e.to_string()))?;
        }
        let mut presets = self.read_presets();
        if presets.remove(id).is_some() {
            self.write_presets(&presets)?;
        }
        Ok(json!({}))
    }

    /// An SVG added to the library: a file written from a provided pattern becomes one of
    /// your presets of that pattern; any other drawing becomes one of your patterns.
    pub fn import_svg(&self, path: &Path, name: Option<&str>) -> Result<Value> {
        let (text, d) = read_user_svg(path)?;
        if let Some(meta) = pattern_of(&text) {
            let pid = meta["id"].as_str().unwrap().to_string();
            let mut title = name.map(String::from).unwrap_or_else(|| d.name.clone());
            if title == library::spec(&pid)?.name {
                title = path
                    .file_stem()
                    .map(|s| s.to_string_lossy().replace('_', " "))
                    .unwrap_or_default();
            }
            let preset = self.save_preset(
                &pid,
                &title,
                meta.get("values"),
                meta.get("directions"),
                None,
            )?;
            return Ok(json!({"id": pid, "preset": preset["id"]}));
        }
        self.save_pattern(
            name.unwrap_or(&d.name),
            &text,
            &d.form,
            Some(&d.groups),
            None,
        )
    }

    /// An image (SVG, PNG or JPEG) added to the library as an image pattern
    /// (ADR 0015): the image kept in the pattern file, read by its parameters.
    pub fn import_image(&self, path: &Path, name: Option<&str>) -> Result<Value> {
        let ext = path
            .extension()
            .map(|e| e.to_string_lossy().to_lowercase())
            .unwrap_or_default();
        if !IMAGE_EXTS.contains(&ext.as_str()) {
            return err(format!(
                "an image must be an SVG, PNG or JPEG file, not .{ext}"
            ));
        }
        let (href, aspect) = image_reader()?.data_url(path).map_err(Error)?;
        let name = name
            .map(str::trim)
            .filter(|n| !n.is_empty())
            .map(String::from)
            .unwrap_or_else(|| {
                path.file_stem()
                    .map(|s| s.to_string_lossy().replace(['_', '-'], " "))
                    .unwrap_or_else(|| "My image".into())
            });
        let d = Definition {
            name: name.clone(),
            form: image_form(ext != "svg"),
            groups: image_groups(),
            image: true,
        };
        let (w, h) = (100.0, round_to(100.0 * aspect, 3));
        let text = format!(
            "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{w}mm\" height=\"{h}mm\" viewBox=\"0 0 {w} {h}\">\n\
             <title>{}</title>\n<image width=\"{w}\" height=\"{h}\" href=\"{href}\"/>\n</svg>\n",
            html_escape_noquote(&name)
        );
        let text = with_metadata(&text, &d)?;
        check_drawing(&render_pattern(&text, &d, &form::defaults(&d.form))?)?;
        let id = self.new_id(&name);
        let out = self.path_of(&id)?;
        fs::create_dir_all(&self.dir)
            .and_then(|_| fs::write(&out, text))
            .map_err(|e| Error(e.to_string()))?;
        Ok(json!({"id": id}))
    }

    /// Whether a pattern reference is an image pattern (a Pro feature).
    pub fn is_image(&self, r: &Value) -> bool {
        if r.get("image").is_some() {
            return true;
        }
        let Some(id) = r.get("id").and_then(Value::as_str) else {
            return false;
        };
        if library::spec(id).is_ok() {
            return false;
        }
        self.path_of(id)
            .ok()
            .and_then(|p| read_user_svg(&p).ok())
            .is_some_and(|(_, d)| d.image)
    }

    /// Create (no id) or update one of the user's presets for a pattern.
    pub fn save_preset(
        &self,
        id: &str,
        name: &str,
        values: Option<&Value>,
        directions: Option<&Value>,
        preset_id: Option<&str>,
    ) -> Result<Value> {
        let name = name.trim();
        if name.is_empty() {
            return err("give the preset a name");
        }
        let d = self.definition(id)?;
        let values = form::check(&d["form"], values, d["name"].as_str().unwrap_or(""))?;
        let directions = check_directions_any(directions)?;
        let mut presets = self.read_presets();
        let mine = presets
            .entry(id.to_string())
            .or_insert_with(|| json!([]))
            .as_array_mut()
            .unwrap();
        let out_id = match preset_id {
            None => {
                let mut stem = String::new();
                let mut gap = false;
                for c in name.chars() {
                    if c.is_ascii_alphanumeric() {
                        if gap && !stem.is_empty() {
                            stem.push('-');
                        }
                        gap = false;
                        stem.push(c.to_ascii_lowercase());
                    } else {
                        gap = true;
                    }
                }
                if stem.is_empty() {
                    stem = "preset".into();
                }
                let mut taken: BTreeSet<String> = mine
                    .iter()
                    .filter_map(|p| p["id"].as_str().map(String::from))
                    .collect();
                taken.insert("default".into());
                if let Ok(s) = library::spec(id) {
                    taken.extend(s.presets.iter().map(|p| p.0.clone()));
                }
                let (mut pid, mut k) = (stem.clone(), 1);
                while taken.contains(&pid) {
                    k += 1;
                    pid = format!("{stem}-{k}");
                }
                mine.push(
                    json!({"id": pid, "name": name, "values": values, "directions": directions}),
                );
                pid
            }
            Some(pid) => {
                let Some(p) = mine.iter_mut().find(|p| p["id"] == pid) else {
                    return err(format!("you have no preset {pid} for this pattern"));
                };
                p["name"] = json!(name);
                p["values"] = Value::Object(values);
                p["directions"] = Value::Object(directions);
                pid.to_string()
            }
        };
        self.write_presets(&presets)?;
        Ok(json!({"id": out_id}))
    }

    pub fn rename_preset(&self, id: &str, preset_id: &str, name: &str) -> Result<Value> {
        let mut presets = self.read_presets();
        if let Some(list) = presets.get_mut(id).and_then(Value::as_array_mut) {
            if let Some(p) = list.iter_mut().find(|p| p["id"] == preset_id) {
                let n = name.trim();
                if !n.is_empty() {
                    p["name"] = json!(n);
                }
                self.write_presets(&presets)?;
                return Ok(json!({}));
            }
        }
        err(format!("you have no preset {preset_id} for this pattern"))
    }

    pub fn delete_preset(&self, id: &str, preset_id: &str) -> Result<Value> {
        let mut presets = self.read_presets();
        let mine = presets
            .get(id)
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let kept: Vec<Value> = mine
            .iter()
            .filter(|p| p["id"] != preset_id)
            .cloned()
            .collect();
        if kept.len() == mine.len() {
            return err(format!("you have no preset {preset_id} for this pattern"));
        }
        presets.insert(id.to_string(), Value::Array(kept));
        self.write_presets(&presets)?;
        Ok(json!({}))
    }
}

fn check_directions_any(directions: Option<&Value>) -> Result<Map<String, Value>> {
    let mut out = Map::new();
    if let Some(Value::Object(m)) = directions {
        for (k, v) in m {
            match v {
                Value::String(s) if s == "rings" => {
                    out.insert(k.clone(), json!("rings"));
                }
                Value::Number(n) => {
                    out.insert(k.clone(), json!(n.as_f64().unwrap()));
                }
                _ => return err("a direction is degrees or \"rings\""),
            }
        }
    }
    Ok(out)
}

/// `json.dumps(v, indent=n, sort_keys=True)` as Python writes it.
pub fn py_json_indent(v: &Value, n: usize) -> String {
    py_json_pretty(v, n, true)
}

/// `json.dumps(v, indent=n[, sort_keys=True])` as Python writes it.
pub fn py_json_pretty(v: &Value, n: usize, sort: bool) -> String {
    fn go(v: &Value, n: usize, sort: bool, depth: usize, out: &mut String) {
        let pad = |d: usize| " ".repeat(n * d);
        match v {
            Value::Array(a) if !a.is_empty() => {
                out.push_str("[\n");
                for (i, x) in a.iter().enumerate() {
                    out.push_str(&pad(depth + 1));
                    go(x, n, sort, depth + 1, out);
                    out.push_str(if i + 1 < a.len() { ",\n" } else { "\n" });
                }
                out.push_str(&pad(depth));
                out.push(']');
            }
            Value::Object(m) if !m.is_empty() => {
                let mut keys: Vec<&String> = m.keys().collect();
                if sort {
                    keys.sort();
                }
                out.push_str("{\n");
                for (i, k) in keys.iter().enumerate() {
                    out.push_str(&pad(depth + 1));
                    out.push_str(&py_json(&Value::String((*k).clone())));
                    out.push_str(": ");
                    go(&m[*k], n, sort, depth + 1, out);
                    out.push_str(if i + 1 < keys.len() { ",\n" } else { "\n" });
                }
                out.push_str(&pad(depth));
                out.push('}');
            }
            other => out.push_str(&py_json(other)),
        }
    }
    let mut out = String::new();
    go(v, n, sort, 0, &mut out);
    out
}

/// (pattern id, values) for a version-1 generator (`{"family", "params"}`).
pub fn from_old_generator(meta: &Value) -> Option<(String, Map<String, Value>)> {
    let family = meta.get("family")?.as_str()?;
    let mut params = meta
        .get("params")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let reg = library::registry();
    if let Some((_, pid, extra)) = reg.old_families.iter().find(|(f, _, _)| f == family) {
        params.extend(extra.clone());
        return Some((pid.clone(), params));
    }
    library::spec(family)
        .is_ok()
        .then(|| (family.to_string(), params))
}

/// The provided pattern a pattern SVG was made from: `{"id", "values"[, "directions"]}`.
/// Reads data-pattern, and the data-generator of files written before patterns had presets.
pub fn pattern_of(text: &str) -> Option<Value> {
    let opts = roxmltree::ParsingOptions {
        allow_dtd: true,
        ..Default::default()
    };
    let doc = roxmltree::Document::parse_with_options(text, opts).ok()?;
    let root = doc.root_element();
    if let Some(raw) = root.attribute("data-pattern") {
        let meta: Value = serde_json::from_str(raw).ok()?;
        return library::spec(meta.get("id")?.as_str()?)
            .is_ok()
            .then_some(meta);
    }
    let raw = root.attribute("data-generator")?;
    let old: Value = serde_json::from_str(raw).ok()?;
    let (pid, params) = from_old_generator(&old)?;
    let mut out = json!({"id": pid, "values": params});
    if let Some(d) = old
        .get("directions")
        .filter(|d| d.as_object().is_some_and(|m| !m.is_empty()))
    {
        out["directions"] = d.clone();
    }
    Some(out)
}

// --------------------------------------------------------------------------- //
// Pattern references (what a project holds)
// --------------------------------------------------------------------------- //

/// A pattern reference with its values checked (and defaulted).
pub fn check_ref(r: &Value, lib: &Library) -> Result<Value> {
    const SHAPES: &str =
        "a pattern is {\"id\": ..., \"values\": {...}}, {\"file\": path} or {\"image\": path, \"values\": {...}}";
    let Some(m) = r.as_object() else {
        return err(SHAPES);
    };
    if ["file", "id", "image"]
        .iter()
        .filter(|k| m.contains_key(**k))
        .count()
        != 1
    {
        return err(SHAPES);
    }
    if let Some(f) = m.get("file") {
        return Ok(json!({"file": f}));
    }
    if let Some(f) = m.get("image") {
        let Some(path) = f.as_str() else {
            return err("pattern image must be the image file's path");
        };
        let values = m.get("values").filter(|v| !v.is_null());
        let form = image_form(is_bitmap(path));
        return Ok(json!({"image": path, "values": form::check(&form, values, "image")?}));
    }
    let Some(pid) = m["id"].as_str() else {
        return err("pattern id must be a string");
    };
    let values = m.get("values").filter(|v| !v.is_null());
    if library::spec(pid).is_ok() {
        return Ok(json!({"id": pid, "values": library::check_values(pid, values)?}));
    }
    let is_pack = pack_parts(pid).is_some();
    if !pid.starts_with(USER) && !is_pack {
        return err(format!("unknown pattern '{pid}'"));
    }
    let mut out = match lib.definition(pid) {
        // not in this library (another computer): keep the values as they are
        Err(_) => match values.filter(|v| form::truthy(Some(v))) {
            None => json!({"id": pid, "values": {}}),
            Some(v @ Value::Object(_)) => json!({"id": pid, "values": v}),
            Some(_) => return err(format!("{pid} values must be an object")),
        },
        Ok(d) => json!({"id": pid, "values": form::check(&d["form"], values, pid)?}),
    };
    // a pack pattern keeps its pack's details, to say what to install where it's missing
    if is_pack {
        let meta = pack_parts(pid)
            .and_then(|(p, _)| lib.pack_meta(p))
            .or_else(|| m.get("pack").filter(|v| v.is_object()).cloned());
        if let Some(meta) = meta {
            let keep = ["id", "name", "version", "author", "homepage"];
            let slim: Map<String, Value> = meta
                .as_object()
                .into_iter()
                .flatten()
                .filter(|(k, _)| keep.contains(&k.as_str()))
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect();
            out["pack"] = Value::Object(slim);
        }
    }
    Ok(out)
}

/// One repeat as SVG text for the reference. Provided patterns are drawn with
/// `directions` baked in; others are used as drawn.
pub fn resolve(r: Option<&Value>, lib: &Library, directions: Option<&Value>) -> Result<String> {
    let Some(r) = r.filter(|r| !r.is_null()) else {
        return err("choose a pattern first");
    };
    if let Some(f) = r.get("file").and_then(Value::as_str) {
        return fs::read_to_string(f).map_err(|_| Error(format!("pattern not found: {f}")));
    }
    if let Some(f) = r.get("image").and_then(Value::as_str) {
        let form = image_form(is_bitmap(f));
        let values = form::check(&form, r.get("values").filter(|v| !v.is_null()), "image")?;
        if !Path::new(f).is_file() {
            return err(format!("image not found: {f}"));
        }
        return image_reader()?
            .page(Image::File(Path::new(f)), &values)
            .map_err(Error);
    }
    let pid = r["id"].as_str().unwrap_or("");
    let values = r.get("values").filter(|v| !v.is_null());
    if library::spec(pid).is_ok() {
        return Ok(library::generate(pid, values, directions, None, None)?.0);
    }
    let path = lib.path_of(pid)?;
    if !path.exists() {
        if let Some((pack, name)) = pack_parts(pid) {
            let m = &r["pack"];
            let s = |k: &str| m[k].as_str().unwrap_or("").to_string();
            let mut what = format!(
                "the pack {}",
                if s("name").is_empty() {
                    pack.to_string()
                } else {
                    s("name")
                }
            );
            if !s("version").is_empty() {
                what += &format!(" {}", s("version"));
            }
            if !s("author").is_empty() {
                what += &format!(" by {}", s("author"));
            }
            if !s("homepage").is_empty() {
                what += &format!(" ({})", s("homepage"));
            }
            return err(format!(
                "this project uses the pattern {name} from {what}: install that pack"
            ));
        }
        return err(format!(
            "pattern not found: {} (it's in another computer's library?)",
            &pid[USER.len().min(pid.len())..]
        ));
    }
    let (text, d) = read_user_svg(&path)?;
    let values = form::check(&d.form, values, &d.name)?;
    render_pattern(&text, &d, &values)
}

/// Override tile groups' line directions on a tile drawing. `directions` are in
/// the pattern's frame; `rotation` is how far the pattern was turned on the assembly.
pub fn apply_directions(
    d: &mut TileDrawing,
    directions: Option<&Map<String, Value>>,
    rotation: f64,
) {
    let Some(dirs) = directions else { return };
    for t in d.tiles.iter_mut() {
        t.direction = match dirs.get(&t.color).and_then(Direction::from_json) {
            Some(Direction::Concentric) => Direction::Concentric,
            Some(Direction::Angle(a)) => Direction::Angle(a + rotation),
            None => continue,
        };
        t.source = crate::drawing::Source::Colour;
    }
}

/// Directions to bake into a generated file (provided patterns take them directly).
pub fn baked_directions<'a>(r: Option<&Value>, directions: Option<&'a Value>) -> Option<&'a Value> {
    let id = r.and_then(|r| r.get("id")).and_then(Value::as_str)?;
    library::spec(id).is_ok().then_some(directions).flatten()
}

fn labels(r: &Value, lib: &Library) -> Map<String, Value> {
    if r.get("image").is_some() {
        return image_groups();
    }
    let id = r.get("id").and_then(Value::as_str).unwrap_or("");
    if let Ok(s) = library::spec(id) {
        return s
            .groups
            .iter()
            .map(|g| (g["group"].as_str().unwrap().to_string(), g["label"].clone()))
            .collect();
    }
    if id.starts_with(USER) || id.starts_with(PACK) {
        if let Ok(p) = lib.path_of(id) {
            if let Ok((_, d)) = read_user_svg(&p) {
                return d.groups;
            }
        }
    }
    Map::new()
}

fn title_of(text: &str, path: &str) -> String {
    let opts = roxmltree::ParsingOptions {
        allow_dtd: true,
        ..Default::default()
    };
    if let Ok(doc) = roxmltree::Document::parse_with_options(text, opts) {
        for el in doc.root_element().children().filter(|n| n.is_element()) {
            if el.tag_name().name().ends_with("title") {
                let t: String = el.children().filter_map(|c| c.text()).collect();
                if !t.is_empty() {
                    return t.trim().to_string();
                }
            }
        }
    }
    Path::new(path)
        .file_stem()
        .map(|s| s.to_string_lossy().replace('_', " "))
        .unwrap_or_default()
}

pub fn name_of(r: &Value, lib: &Library) -> String {
    let id = r.get("id").and_then(Value::as_str).unwrap_or("");
    if let Ok(s) = library::spec(id) {
        return s.name.clone();
    }
    if let Some((_, name)) = pack_parts(id) {
        return lib
            .path_of(id)
            .ok()
            .and_then(|p| read_user_svg(&p).ok())
            .map(|(_, d)| d.name)
            .unwrap_or_else(|| name.to_string());
    }
    if let Some(name) = id.strip_prefix(USER) {
        return lib
            .path_of(id)
            .ok()
            .and_then(|p| read_user_svg(&p).ok())
            .map(|(_, d)| d.name)
            .unwrap_or_else(|| name.to_string());
    }
    if let Some(f) = r.get("image").and_then(Value::as_str) {
        return Path::new(f)
            .file_stem()
            .map(|s| s.to_string_lossy().replace(['_', '-'], " "))
            .unwrap_or_default();
    }
    let f = r.get("file").and_then(Value::as_str).unwrap_or("");
    title_of(&fs::read_to_string(f).unwrap_or_default(), f)
}

/// Name, repeat size and tile groups (with their directions) of a pattern at its values.
pub fn info(r: &Value, lib: &Library) -> Result<Value> {
    let text = resolve(Some(r), lib, None)?;
    let d = tiles_of(&text)?;
    let mut groups: Vec<Value> = Vec::new();
    for t in &d.tiles {
        match groups.iter_mut().find(|g| g["group"] == t.color.as_str()) {
            Some(g) => g["tiles"] = json!(g["tiles"].as_i64().unwrap() + 1),
            None => groups.push(json!({"group": t.color, "tiles": 1, "direction": dir_json(t)})),
        }
    }
    let labels = labels(r, lib);
    for (k, g) in groups.iter_mut().enumerate() {
        let c = g["group"].as_str().unwrap().to_string();
        g["label"] = labels
            .get(&c)
            .filter(|l| l.as_str().is_some_and(|s| !s.is_empty()))
            .cloned()
            .unwrap_or(json!(format!("Group {}", k + 1)));
    }
    let mut out = json!({"name": name_of(r, lib), "size": [round_to(d.size.0, 4), round_to(d.size.1, 4)], "groups": groups});
    if lib.is_image(r) {
        out["image"] = json!(true);
    }
    // an image file named by the project has no library entry: its parameters' form
    if let Some(f) = r.get("image").and_then(Value::as_str) {
        out["form"] = image_form(is_bitmap(f));
    }
    Ok(out)
}

/// One repeat for previews: page size and tiles (pattern mm, y up).
pub fn geometry_of_text(text: &str, directions: Option<&Value>) -> Result<Value> {
    let mut d = tiles_of(text)?;
    apply_directions(&mut d, directions.and_then(Value::as_object), 0.0);
    let ring = |r: &geo::LineString<f64>| -> Value {
        let c = &r.0[..r.0.len().saturating_sub(1)];
        Value::Array(
            c.iter()
                .map(|p| json!([round_to(p.x, 4), round_to(p.y, 4)]))
                .collect(),
        )
    };
    let tiles: Vec<Value> = d
        .tiles
        .iter()
        .map(|t| {
            let mut rings = vec![ring(t.poly.exterior())];
            rings.extend(t.poly.interiors().iter().map(ring));
            let (angle, concentric) = match t.direction {
                Direction::Angle(a) => (json!(round_to(a, 4)), false),
                Direction::Concentric => (Value::Null, true),
            };
            json!({"group": t.color, "angle": angle, "concentric": concentric, "rings": rings})
        })
        .collect();
    Ok(json!({"size": [round_to(d.size.0, 4), round_to(d.size.1, 4)], "tiles": tiles}))
}

pub fn geometry(r: Option<&Value>, directions: Option<&Value>, lib: &Library) -> Result<Value> {
    let text = resolve(r, lib, baked_directions(r, directions))?;
    geometry_of_text(&text, directions)
}

/// Geometry of an unsaved user pattern (the pattern editor's live preview).
pub fn preview_source(
    source: &str,
    form_v: &Value,
    values: Option<&Value>,
    directions: Option<&Value>,
) -> Result<Value> {
    form::validate_form(form_v)?;
    let defaults = Value::Object(form::defaults(form_v));
    let values = form::check(
        form_v,
        Some(
            values
                .filter(|v| v.as_object().is_some_and(|m| !m.is_empty()))
                .unwrap_or(&defaults),
        ),
        "pattern",
    )?;
    let unknown: Vec<String> = expr::names_used(source)
        .into_iter()
        .filter(|n| !values.contains_key(n))
        .collect();
    if !unknown.is_empty() {
        return err(format!(
            "the drawing uses {}, which {} not a parameter",
            unknown.join(", "),
            if unknown.len() == 1 { "is" } else { "are" }
        ));
    }
    geometry_of_text(&render_user_svg(source, &values)?, directions)
}

/// The pattern at its values as plain SVG text (to share, or for the command line).
pub fn export(
    r: Option<&Value>,
    directions: Option<&Value>,
    title: Option<&str>,
    lib: &Library,
) -> Result<String> {
    let Some(r) = r.filter(|r| !r.is_null()) else {
        return err("choose a pattern first");
    };
    if let Some(id) = r.get("id").and_then(Value::as_str) {
        if library::spec(id).is_ok() {
            return Ok(library::generate(id, r.get("values"), directions, title, None)?.0);
        }
    }
    let text = resolve(Some(r), lib, None)?;
    if directions.is_some_and(|d| d.as_object().is_some_and(|m| !m.is_empty())) {
        return err("changed line directions can't be written into a drawn pattern; change its light red lines instead");
    }
    Ok(text)
}

pub const STARTER: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" width="{{ brick * 2 }}mm" height="{{ height * 2 }}mm"
     viewBox="0 0 {{ brick * 2 }} {{ height * 2 }}">
<title>My pattern</title>
<!-- One repeat, 1:1 in mm. Fill colour = tile group; blue stroke = grout width;
     a light red line through a tile = its line direction (a closed loop = rings).
     Numbers can be {{ expressions }} using the parameters. -->
<g stroke="#0000ff" stroke-width="{{ grout }}">
  <rect fill="#d8b07a" x="0" y="0" width="{{ brick }}" height="{{ height }}"/>
  <rect fill="#d8b07a" x="{{ brick }}" y="0" width="{{ brick }}" height="{{ height }}"/>
  <rect fill="#9c6a3c" x="{{ -brick / 2 }}" y="{{ height }}" width="{{ brick }}" height="{{ height }}"/>
  <rect fill="#9c6a3c" x="{{ brick / 2 }}" y="{{ height }}" width="{{ brick }}" height="{{ height }}"/>
  <rect fill="#9c6a3c" x="{{ brick * 1.5 }}" y="{{ height }}" width="{{ brick }}" height="{{ height }}"/>
</g>
<g fill="none" stroke="#ff8080" stroke-width="0.5">
  <line x1="{{ brick * 0.2 }}" y1="{{ height / 2 }}" x2="{{ brick * 0.8 }}" y2="{{ height / 2 }}"/>
  <line x1="{{ brick }}" y1="{{ height * 1.2 }}" x2="{{ brick }}" y2="{{ height * 1.8 }}"/>
</g>
</svg>
"##;

/// The starter pattern's form and tile group labels.
pub fn starter() -> (Value, Map<String, Value>) {
    let number = |key: &str,
                  label: &str,
                  default: f64,
                  lo: f64,
                  hi: f64,
                  step: f64,
                  desc: Option<&str>| {
        let mut c = json!({"type": "number", "key": key, "label": label, "input": true, "defaultValue": default,
            "validate": {"required": true, "min": lo, "max": hi, "integer": false},
            "decimalLimit": 3, "requireDecimal": false, "delimiter": false,
            "properties": {"step": svg::fmt_g(step)}, "suffix": "mm"});
        if let Some(d) = desc {
            c["description"] = json!(d);
        }
        c
    };
    let form = json!({"components": [
        number("brick", "Brick length", 16.0, 4.0, 80.0, 0.5, None),
        number("height", "Brick height", 8.0, 2.0, 40.0, 0.5, None),
        number("grout", "Grout", 0.4, 0.0, 3.0, 0.05, Some("The groove between tiles that shows each tile.")),
    ]});
    let mut groups = Map::new();
    groups.insert("#d8b07a".into(), json!("Light bricks"));
    groups.insert("#9c6a3c".into(), json!("Dark bricks"));
    (form, groups)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp() -> PathBuf {
        let d =
            std::env::temp_dir().join(format!("pp-lib-{}-{}", std::process::id(), rand_suffix()));
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn rand_suffix() -> u128 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    }

    #[test]
    fn user_pattern_with_parameters_and_presets() {
        let lib = Library::new(Some(tmp()));
        let (form, groups) = starter();
        let id = lib
            .save_pattern("Bricks", STARTER, &form, Some(&groups), None)
            .unwrap();
        assert_eq!(id["id"], "user:bricks");
        let listed = lib.list().unwrap();
        let mine = listed.iter().find(|p| p["id"] == "user:bricks").unwrap();
        assert_eq!(mine["provided"], false);
        assert_eq!(mine["parametric"], true);
        let labels: Vec<&str> = mine["groups"]
            .as_array()
            .unwrap()
            .iter()
            .map(|g| g["label"].as_str().unwrap())
            .collect();
        assert_eq!(labels, ["Light bricks", "Dark bricks"]);
        let p = lib
            .save_preset(
                "user:bricks",
                "Long",
                Some(&json!({"brick": 30})),
                None,
                None,
            )
            .unwrap();
        assert_eq!(p["id"], "long");
        let p2 = lib
            .save_preset(
                "user:bricks",
                "Long",
                Some(&json!({"brick": 31})),
                None,
                None,
            )
            .unwrap();
        assert_eq!(p2["id"], "long-2");
        lib.rename_preset("user:bricks", "long", "Longer").unwrap();
        lib.delete_preset("user:bricks", "long-2").unwrap();
        let listed = lib.list().unwrap();
        let mine = listed.iter().find(|p| p["id"] == "user:bricks").unwrap();
        let names: Vec<&str> = mine["presets"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| p["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, ["Default", "Longer"]);
        let r = check_ref(&json!({"id": "user:bricks", "values": {"brick": 20}}), &lib).unwrap();
        assert_eq!(r["values"]["height"], json!(8.0));
        let info = info(&r, &lib).unwrap();
        assert_eq!(info["size"], json!([40.0, 16.0]));
        assert!(lib.save_pattern("Bad", "<svg xmlns='http://www.w3.org/2000/svg'><rect width='{{ nope }}' height='1'/></svg>", &form, None, None)
            .unwrap_err().0.contains("nope"));
        lib.delete_pattern("user:bricks").unwrap();
        assert!(lib.user_ids().is_empty());
    }

    #[test]
    fn provided_presets_import_as_presets() {
        let dir = tmp();
        let lib = Library::new(Some(dir.clone()));
        let (text, _) = library::generate(
            "chevron",
            Some(&json!({"column_width": 20})),
            None,
            None,
            None,
        )
        .unwrap();
        let f = dir.join("wide_chevron.svg.in");
        fs::write(&f, text).unwrap();
        let r = lib.import_svg(&f, None).unwrap();
        assert_eq!(r["id"], "chevron");
        assert_eq!(r["preset"], "wide-chevron-svg");
    }

    #[test]
    fn metadata_round_trips() {
        let d = Definition {
            name: "A & B".into(),
            form: json!({"components": []}),
            groups: Map::new(),
            image: false,
        };
        let t = with_metadata("<svg xmlns=\"http://www.w3.org/2000/svg\">\n<metadata id=\"tessafab\">{}</metadata><rect/></svg>", &d).unwrap();
        assert_eq!(t.matches("<metadata").count(), 1);
        assert_eq!(definition_of(&t, "x").unwrap().name, "A & B");
    }
}
