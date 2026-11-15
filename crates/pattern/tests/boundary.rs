//! The pattern crate knows no file format or craft (ADR 0008): sources and
//! targets (the G-code engine today) build on it, never the other way round.
//! Cargo already refuses a dependency cycle with tessafab-core; these checks
//! also keep G-code code from being added here or pulled in some other way.
use std::fs;
use std::path::{Path, PathBuf};

fn crate_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for e in fs::read_dir(dir).unwrap() {
        let p = e.unwrap().path();
        if p.is_dir() {
            rust_files(&p, out);
        } else if p.extension().is_some_and(|x| x == "rs") {
            out.push(p);
        }
    }
}

/// The text of a Rust file without its `//` comments (doc comments included),
/// so prose may still mention G-code.
fn code_of(text: &str) -> String {
    text.lines()
        .map(|l| match l.find("//") {
            Some(i) => &l[..i],
            None => l,
        })
        .collect::<Vec<_>>()
        .join("\n")
        .to_lowercase()
}

#[test]
fn no_gcode_code_in_the_pattern_crate() {
    const WORDS: &[&str] = &[
        "gcode",
        "bgcode",
        "slicer",
        "toolpath",
        "extrusion",
        "filament",
        "tessafab_core",
        "tessafab_pro",
    ];
    let mut files = Vec::new();
    rust_files(&crate_dir().join("src"), &mut files);
    assert!(!files.is_empty());
    let this = Path::new(file!()).file_name().unwrap();
    rust_files(&crate_dir().join("tests"), &mut files);
    let bad: Vec<String> = files
        .iter()
        .filter(|p| p.file_name() != Some(this))
        .flat_map(|p| {
            let code = code_of(&fs::read_to_string(p).unwrap());
            WORDS
                .iter()
                .filter(move |w| code.contains(*w))
                .map(move |w| format!("{} uses {w:?}", p.display()))
                .collect::<Vec<_>>()
        })
        .collect();
    assert!(bad.is_empty(), "G-code code in the pattern crate: {bad:#?}");
}

#[test]
fn only_format_neutral_dependencies() {
    // Geometry, SVG and JSON. Adding one here is a deliberate decision.
    const ALLOWED: &[&str] = &["geo", "i_overlay", "serde", "serde_json", "roxmltree"];
    let manifest = fs::read_to_string(crate_dir().join("Cargo.toml")).unwrap();
    let mut section = "";
    let mut bad = Vec::new();
    for line in manifest.lines().map(str::trim) {
        if line.starts_with('[') {
            section = line;
            continue;
        }
        let deps = section.ends_with("dependencies]");
        if let (true, Some((name, _))) = (deps, line.split_once('=')) {
            let name = name.trim().split('.').next().unwrap().trim();
            if !name.is_empty() && !name.starts_with('#') && !ALLOWED.contains(&name) {
                bad.push(name.to_string());
            }
        }
    }
    assert!(
        bad.is_empty(),
        "dependencies outside the allowed list: {bad:?}"
    );
}
