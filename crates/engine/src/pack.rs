//! Pattern packs (ADR 0011, M23): patterns shared as `.tessafab-pack` files.
//!
//! A pack is a zip holding `pack.json` (`id`, `name`, `version`, `author`,
//! `licence`, `homepage`, `minTessaFab`), `patterns/<name>.svg` (user pattern
//! SVGs with their parameter forms), `presets.json` (`{"patterns": {name:
//! [presets]}}`) and `previews/<name>.svg`. It is data only: importing one
//! can't run code. Anything but those files, paths outside the pack, files or
//! packs too large, and SVGs with scripts, foreign objects, event attributes
//! or links outside the file are refused, and every pattern must render at its
//! defaults before anything is installed. Installed packs live in the
//! library's `packs/<id>/`; their patterns are read-only (`pack:<id>/<name>`).
//! Packs work in every edition; selling them is commercial use.

use crate::form;
use crate::patterns::{self, Library, PACK, USER};
use serde_json::{json, Map, Value};
use std::io::{Read, Write};
use std::path::Path;

/// The largest file in a pack, and the largest pack (uncompressed bytes).
pub const MAX_FILE: u64 = 1 << 20;
pub const MAX_TOTAL: u64 = 20 << 20;
pub const MAX_PATTERNS: usize = 500;

#[derive(Debug, Clone, PartialEq)]
pub struct Error(pub String);

pub type Result<T> = std::result::Result<T, Error>;

fn err<T>(m: impl Into<String>) -> Result<T> {
    Err(Error(m.into()))
}

/// A version's numbers (`1.2.3`, `0.1.0-beta.8` -> [0, 1, 0]).
fn numbers(v: &str) -> Vec<u64> {
    v.split(['-', '+'])
        .next()
        .unwrap_or("")
        .split('.')
        .map(|n| n.parse().unwrap_or(0))
        .collect()
}

/// `pack.json`, checked; the same rules for importing and exporting.
pub fn check_meta(meta: &Value) -> Result<Map<String, Value>> {
    let Some(m) = meta.as_object() else {
        return err("pack.json must be an object");
    };
    let s = |k: &str| {
        m.get(k)
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_string()
    };
    let id = s("id");
    if id.is_empty() || id.len() > 64 || !patterns::safe_name(&id) {
        return err("the pack id must be 1 to 64 letters, digits, '-' or '_'");
    }
    if s("name").is_empty() || s("name").len() > 120 {
        return err("give the pack a name (up to 120 characters)");
    }
    if s("version").is_empty() || s("version").len() > 32 {
        return err("give the pack a version, such as 1.0.0");
    }
    let home = s("homepage");
    if !home.is_empty() && !(home.starts_with("https://") || home.starts_with("http://")) {
        return err("the pack's home page must be a web address (https://...)");
    }
    let min = s("minTessaFab");
    if !min.is_empty() && numbers(&min) > numbers(crate::VERSION) {
        return err(format!(
            "this pack needs TessaFab {min} or newer (this is {})",
            crate::VERSION
        ));
    }
    let mut out = Map::new();
    for k in [
        "id",
        "name",
        "version",
        "author",
        "licence",
        "homepage",
        "minTessaFab",
        "description",
    ] {
        if !s(k).is_empty() {
            out.insert(k.into(), json!(s(k)));
        }
    }
    Ok(out)
}

/// What an SVG in a pack must not hold: anything that runs, or reaches outside the file.
fn svg_problem(text: &str) -> Option<&'static str> {
    let low = text.to_ascii_lowercase();
    if low.contains("<script") {
        return Some("a script");
    }
    if low.contains("<foreignobject") {
        return Some("a foreign object");
    }
    if low.contains("javascript:") {
        return Some("a javascript: link");
    }
    if low.contains("<!entity") || low.contains("<!doctype") {
        return Some("a document type or entity");
    }
    // event attributes: on...= inside a tag
    let b = low.as_bytes();
    for i in 1..b.len().saturating_sub(3) {
        if b[i - 1].is_ascii_whitespace() && b[i] == b'o' && b[i + 1] == b'n' {
            let rest = &low[i + 2..];
            let name: String = rest
                .chars()
                .take_while(|c| c.is_ascii_alphabetic())
                .collect();
            if !name.is_empty() && rest[name.len()..].trim_start().starts_with('=') {
                return Some("an event attribute");
            }
        }
    }
    for attr in ["href=\"", "href='", "src=\"", "src='"] {
        for (i, _) in low.match_indices(attr) {
            let v = &low[i + attr.len()..];
            if !v.starts_with('#') {
                return Some("a link outside the file");
            }
        }
    }
    if low.contains("url(")
        && !low
            .split("url(")
            .skip(1)
            .all(|r| r.trim_start().starts_with('#'))
    {
        return Some("a link outside the file");
    }
    None
}

/// What the pack holds, read and checked.
struct Read_ {
    meta: Map<String, Value>,
    patterns: Vec<(String, String)>,
    previews: Vec<(String, String)>,
    presets: Option<Value>,
}

fn read_zip(path: &Path) -> Result<Read_> {
    let file = std::fs::File::open(path)
        .map_err(|e| Error(format!("can't open {}: {e}", path.display())))?;
    let mut z = zip::ZipArchive::new(file)
        .map_err(|_| Error("this isn't a TessaFab pack (not a zip file)".into()))?;
    if z.len() > 2 * MAX_PATTERNS + 4 {
        return err("the pack holds too many files");
    }
    let (mut meta, mut presets) = (None, None);
    let (mut patterns, mut previews) = (Vec::new(), Vec::new());
    let mut total = 0u64;
    for i in 0..z.len() {
        let mut f = z
            .by_index(i)
            .map_err(|e| Error(format!("can't read the pack: {e}")))?;
        let name = f
            .name()
            .map_err(|e| Error(format!("can't read the pack: {e}")))?
            .to_string();
        if f.is_dir() {
            continue;
        }
        if f.enclosed_name().is_none()
            || name.contains("..")
            || name.starts_with('/')
            || name.contains('\\')
        {
            return err(format!("the pack has a file outside it ({name})"));
        }
        if f.size() > MAX_FILE {
            return err(format!("{name} is too large for a pack (over 1 MB)"));
        }
        let mut bytes = Vec::new();
        (&mut f)
            .take(MAX_FILE + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| Error(format!("can't read {name}: {e}")))?;
        if bytes.len() as u64 > MAX_FILE {
            return err(format!("{name} is too large for a pack (over 1 MB)"));
        }
        total += bytes.len() as u64;
        if total > MAX_TOTAL {
            return err("the pack is too large (over 20 MB)");
        }
        let text = String::from_utf8(bytes).map_err(|_| Error(format!("{name} isn't text")))?;
        let svg_in = |dir: &str| {
            name.strip_prefix(dir)
                .and_then(|n| n.strip_suffix(".svg"))
                .filter(|n| patterns::safe_name(n))
                .map(String::from)
        };
        match name.as_str() {
            "pack.json" => meta = Some(text),
            "presets.json" => presets = Some(text),
            _ => {
                if let Some(n) = svg_in("patterns/") {
                    patterns.push((n, text));
                } else if let Some(n) = svg_in("previews/") {
                    previews.push((n, text));
                } else {
                    return err(format!("the pack has a file a pack can't hold ({name})"));
                }
            }
        }
    }
    let meta: Value = serde_json::from_str(
        meta.as_deref()
            .ok_or(Error("the pack has no pack.json".into()))?,
    )
    .map_err(|e| Error(format!("pack.json can't be read: {e}")))?;
    let meta = check_meta(&meta)?;
    if patterns.is_empty() {
        return err("the pack has no patterns");
    }
    if patterns.len() > MAX_PATTERNS {
        return err("the pack holds too many patterns");
    }
    for (n, text) in patterns.iter().chain(previews.iter()) {
        if let Some(why) = svg_problem(text) {
            return err(format!("{n}.svg holds {why}, which a pack can't"));
        }
    }
    let presets = match presets {
        None => None,
        Some(t) => Some(
            serde_json::from_str(&t)
                .map_err(|e| Error(format!("presets.json can't be read: {e}")))?,
        ),
    };
    Ok(Read_ {
        meta,
        patterns,
        previews,
        presets,
    })
}

/// Check a pack's patterns and presets: every pattern renders at its defaults.
fn check_patterns(r: &Read_) -> Result<()> {
    for (n, text) in &r.patterns {
        let d = patterns::definition_of(text, n).map_err(|e| Error(format!("{n}: {}", e.0)))?;
        let rendered = patterns::render_user_svg(text, &form::defaults(&d.form))
            .map_err(|e| Error(format!("{n}: {}", e.0)))?;
        patterns::check_drawing(&rendered).map_err(|e| Error(format!("{n}: {}", e.0)))?;
        if let Some(list) = r
            .presets
            .as_ref()
            .and_then(|p| p["patterns"][n.as_str()].as_array())
        {
            for p in list {
                form::check(&d.form, p.get("values"), &d.name).map_err(|e| {
                    Error(format!(
                        "{n}, preset {}: {}",
                        p["name"].as_str().unwrap_or("?"),
                        e.0
                    ))
                })?;
            }
        }
    }
    if let Some(m) = r.presets.as_ref().and_then(|p| p["patterns"].as_object()) {
        if let Some(k) = m.keys().find(|k| !r.patterns.iter().any(|(n, _)| n == *k)) {
            return err(format!(
                "presets.json has presets of {k}, which the pack doesn't hold"
            ));
        }
    }
    Ok(())
}

/// Install a pack file in the library (replacing an installed version).
pub fn import(lib: &Library, path: &Path) -> Result<Value> {
    let r = read_zip(path)?;
    check_patterns(&r)?;
    let id = r.meta["id"].as_str().unwrap().to_string();
    let replaced = lib
        .pack_meta(&id)
        .and_then(|m| m["version"].as_str().map(String::from));
    let dir = lib.packs_dir();
    let tmp = dir.join(format!(".{id}.new"));
    let _ = std::fs::remove_dir_all(&tmp);
    let write = || -> std::io::Result<()> {
        std::fs::create_dir_all(tmp.join("patterns"))?;
        std::fs::create_dir_all(tmp.join("previews"))?;
        std::fs::write(
            tmp.join("pack.json"),
            serde_json::to_string_pretty(&Value::Object(r.meta.clone()))?,
        )?;
        for (n, text) in &r.patterns {
            std::fs::write(tmp.join("patterns").join(format!("{n}.svg")), text)?;
        }
        for (n, text) in &r.previews {
            std::fs::write(tmp.join("previews").join(format!("{n}.svg")), text)?;
        }
        if let Some(p) = &r.presets {
            std::fs::write(tmp.join("presets.json"), serde_json::to_string_pretty(p)?)?;
        }
        let dest = dir.join(&id);
        let _ = std::fs::remove_dir_all(&dest);
        std::fs::rename(&tmp, &dest)
    };
    if let Err(e) = write() {
        let _ = std::fs::remove_dir_all(&tmp);
        return err(format!("can't install the pack: {e}"));
    }
    Ok(
        json!({"id": id, "name": r.meta["name"], "version": r.meta["version"],
              "patterns": r.patterns.len(), "replaced": replaced}),
    )
}

/// Remove an installed pack (presets the user made of its patterns stay).
pub fn remove(lib: &Library, id: &str) -> Result<Value> {
    if !patterns::safe_name(id) || lib.pack_meta(id).is_none() {
        return err(format!("no pack '{id}' is installed"));
    }
    std::fs::remove_dir_all(lib.packs_dir().join(id))
        .map_err(|e| Error(format!("can't remove the pack: {e}")))?;
    Ok(json!({}))
}

/// The installed packs, with their patterns' ids.
pub fn list(lib: &Library) -> Value {
    let ids = lib.pack_ids();
    json!(lib
        .pack_list()
        .into_iter()
        .filter_map(|p| {
            let mut m = lib.pack_meta(&p)?;
            let prefix = format!("{PACK}{p}/");
            m["patterns"] = json!(ids
                .iter()
                .filter(|i| i.starts_with(&prefix))
                .collect::<Vec<_>>());
            Some(m)
        })
        .collect::<Vec<_>>())
}

/// Write the user's patterns `ids` (with their presets and a preview of each)
/// as a pack file.
pub fn export(lib: &Library, path: &Path, ids: &[String], meta: &Value) -> Result<Value> {
    let meta = check_meta(meta)?;
    if ids.is_empty() {
        return err("choose the patterns to put in the pack");
    }
    let mut files: Vec<(String, String)> = vec![(
        "pack.json".into(),
        serde_json::to_string_pretty(&Value::Object(meta)).unwrap(),
    )];
    let mut presets = Map::new();
    for id in ids {
        let Some(name) = id.strip_prefix(USER) else {
            return err(format!(
                "only your own patterns go in a pack ({id} isn't one)"
            ));
        };
        let p = lib.path_of(id).map_err(|e| Error(e.0))?;
        let (text, d) =
            patterns::read_user_svg(&p).map_err(|e| Error(format!("{name}: {}", e.0)))?;
        let preview = patterns::render_user_svg(&text, &form::defaults(&d.form))
            .map_err(|e| Error(format!("{name}: {}", e.0)))?;
        if let Some(why) = svg_problem(&text) {
            return err(format!("{name} holds {why}, which a pack can't"));
        }
        files.push((format!("patterns/{name}.svg"), text));
        if svg_problem(&preview).is_none() {
            files.push((format!("previews/{name}.svg"), preview));
        }
        let own = lib.own_presets(id);
        if !own.is_empty() {
            presets.insert(name.to_string(), Value::Array(own));
        }
    }
    if !presets.is_empty() {
        files.push((
            "presets.json".into(),
            serde_json::to_string_pretty(&json!({"patterns": presets})).unwrap(),
        ));
    }
    let tmp = path.with_extension("tmp");
    let write = || -> std::result::Result<(), String> {
        let mut z = zip::ZipWriter::new(std::fs::File::create(&tmp).map_err(|e| e.to_string())?);
        let opts = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        for (name, text) in &files {
            z.start_file(name.as_str(), opts)
                .map_err(|e| e.to_string())?;
            z.write_all(text.as_bytes()).map_err(|e| e.to_string())?;
        }
        z.finish().map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, path).map_err(|e| e.to_string())
    };
    if let Err(e) = write() {
        let _ = std::fs::remove_file(&tmp);
        return err(format!("can't write {}: {e}", path.display()));
    }
    Ok(json!({"path": path.to_string_lossy(), "patterns": ids.len()}))
}
