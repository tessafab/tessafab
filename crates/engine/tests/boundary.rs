//! The open source crates must never depend on Pro code: they are published
//! as their own repository (github.com/tessafab/tessafab) and build alone.
use std::fs;
use std::path::{Path, PathBuf};

fn core_dir() -> PathBuf {
    // oss/crates: every open source crate
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_path_buf()
}

fn files(dir: &Path, out: &mut Vec<PathBuf>) {
    for e in fs::read_dir(dir).unwrap() {
        let p = e.unwrap().path();
        if p.is_dir() {
            if p.file_name().is_some_and(|n| n != "target") {
                files(&p, out);
            }
        } else if matches!(p.extension().and_then(|x| x.to_str()), Some("rs" | "toml")) {
            out.push(p);
        }
    }
}

#[test]
fn core_never_uses_pro() {
    let mut all = Vec::new();
    files(&core_dir(), &mut all);
    assert!(!all.is_empty());
    let this = Path::new(file!()).file_name().unwrap();
    let bad: Vec<_> = all
        .iter()
        .filter(|p| p.file_name() != Some(this))
        .filter(|p| {
            let t = fs::read_to_string(p).unwrap();
            t.contains("tessafab_pro")
                || t.contains("tessafab-pro")
                || t.contains("../pro")
                || t.contains("pro/")
        })
        .collect();
    assert!(bad.is_empty(), "core files refer to Pro code: {:?}", bad);
}
