//! Pattern packs (ADR 0011, M23): a pack exported from one library imports
//! into another and draws the same tiles; hostile packs are refused; a pack's
//! patterns are read-only; a project names a missing pack.

use serde_json::{json, Value};
use std::io::Write;
use std::path::{Path, PathBuf};
use tessafab_core::pack;
use tessafab_core::patterns::{self, Library};
use tessafab_core::session::Session;

fn scratch(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("tessafab-pack-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn meta() -> Value {
    json!({"id": "bricks-pack", "name": "Bricks", "version": "1.0.0", "author": "A maker",
           "licence": "CC BY 4.0", "homepage": "https://example.com/bricks"})
}

/// A library with one pattern of its own (the starter) and a preset of it.
fn library_with_a_pattern(dir: &Path) -> (Session, String) {
    let s = Session::new(Some(dir.to_path_buf()));
    let st = s.pattern_starter();
    let id = s
        .save_pattern(
            "Bricks",
            st["source"].as_str().unwrap(),
            &st["form"],
            Some(&st["groups"]),
            None,
        )
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    s.save_preset(&id, "Long", &json!({"brick": 30.0}), None, None)
        .unwrap();
    (s, id)
}

#[test]
fn a_pack_round_trips_and_draws_the_same_tiles() {
    let (a, id) = library_with_a_pattern(&scratch("a"));
    let file = scratch("files").join("bricks.tessafab-pack");
    let r = a
        .export_pack(&file, std::slice::from_ref(&id), &meta())
        .unwrap();
    assert_eq!(r["patterns"], 1);

    let b = Session::new(Some(scratch("b")));
    let r = b.import_pack(&file).unwrap();
    assert_eq!(
        (r["id"].as_str(), r["patterns"].as_u64()),
        (Some("bricks-pack"), Some(1))
    );
    let packs = b.packs();
    let pid = packs[0]["patterns"][0].as_str().unwrap().to_string();
    assert!(pid.starts_with("pack:bricks-pack/"), "{pid}");

    // the same tiles from the original and from the pack
    let ga = a.pattern_geometry(&json!({"id": id}), None).unwrap();
    let gb = b.pattern_geometry(&json!({"id": pid}), None).unwrap();
    assert_eq!(ga, gb);
    // listed with its pack, its preset, read-only
    let list = b.patterns().unwrap();
    let p = list
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["id"] == pid.as_str())
        .unwrap();
    assert_eq!(p["pack"]["name"], "Bricks");
    assert_eq!(p["readOnly"], true);
    assert!(p["presets"]
        .as_array()
        .unwrap()
        .iter()
        .any(|x| x["name"] == "Long" && x["provided"] == true));
    let st = b.pattern_starter();
    assert!(b
        .save_pattern(
            "x",
            st["source"].as_str().unwrap(),
            &st["form"],
            None,
            Some(&pid)
        )
        .is_err());
    assert!(b.delete_pattern(&pid).is_err());

    // installing it again replaces it; removing it removes its patterns
    assert_eq!(b.import_pack(&file).unwrap()["replaced"], "1.0.0");
    b.remove_pack("bricks-pack").unwrap();
    assert!(b.packs().as_array().unwrap().is_empty());
}

#[test]
fn a_project_without_its_pack_says_which_to_install() {
    let (a, id) = library_with_a_pattern(&scratch("p-a"));
    let file = scratch("p-files").join("bricks.tessafab-pack");
    a.export_pack(&file, std::slice::from_ref(&id), &meta())
        .unwrap();
    let b = Session::new(Some(scratch("p-b")));
    b.import_pack(&file).unwrap();
    let pid = b.packs()[0]["patterns"][0].as_str().unwrap().to_string();
    let r = patterns::check_ref(&json!({"id": pid}), &b.library).unwrap();
    assert_eq!(r["pack"]["homepage"], "https://example.com/bricks");

    let c = Library::new(Some(scratch("p-c")));
    let kept = patterns::check_ref(&r, &c).unwrap();
    let e = patterns::resolve(Some(&kept), &c, None).unwrap_err().0;
    assert_eq!(
        e,
        format!(
            "this project uses the pattern {} from the pack Bricks 1.0.0 by A maker (https://example.com/bricks): install that pack",
            pid.rsplit('/').next().unwrap()
        )
    );
}

/// A zip with these files.
fn zip_of(path: &Path, files: &[(&str, &str)]) {
    let mut z = zip::ZipWriter::new(std::fs::File::create(path).unwrap());
    for (name, text) in files {
        z.start_file(*name, zip::write::SimpleFileOptions::default())
            .unwrap();
        z.write_all(text.as_bytes()).unwrap();
    }
    z.finish().unwrap();
}

/// A hostile pack: its name, its files, and what the refusal says.
type Case<'a> = (&'a str, Vec<(&'a str, String)>, &'a str);

#[test]
fn hostile_packs_are_refused_and_install_nothing() {
    let (a, id) = library_with_a_pattern(&scratch("h-a"));
    let good = patterns::read_user_svg(&a.library.path_of(&id).unwrap())
        .unwrap()
        .0;
    let m = meta().to_string();
    let dir = scratch("h");
    let lib = Session::new(Some(scratch("h-lib")));
    let cases: Vec<Case> = vec![
        (
            "script",
            vec![
                ("pack.json", m.clone()),
                (
                    "patterns/x.svg",
                    good.replace("</svg>", "<script>alert(1)</script></svg>"),
                ),
            ],
            "holds a script",
        ),
        (
            "outside",
            vec![("pack.json", m.clone()), ("../x.svg", good.clone())],
            "outside it",
        ),
        (
            "extra",
            vec![
                ("pack.json", m.clone()),
                ("patterns/x.svg", good.clone()),
                ("run.sh", "echo hi".into()),
            ],
            "can't hold (run.sh)",
        ),
        (
            "link",
            vec![
                ("pack.json", m.clone()),
                (
                    "patterns/x.svg",
                    good.replace(
                        "</svg>",
                        "<image href=\"https://evil.example/x.png\"/></svg>",
                    ),
                ),
            ],
            "a link outside the file",
        ),
        (
            "event",
            vec![
                ("pack.json", m.clone()),
                (
                    "patterns/x.svg",
                    good.replace("<svg ", "<svg onload=\"x()\" "),
                ),
            ],
            "an event attribute",
        ),
        (
            "big",
            vec![
                ("pack.json", m.clone()),
                ("patterns/x.svg", "x".repeat((pack::MAX_FILE + 10) as usize)),
            ],
            "too large",
        ),
        (
            "newer",
            vec![
                (
                    "pack.json",
                    json!({"id": "n", "name": "N", "version": "1", "minTessaFab": "99.0.0"})
                        .to_string(),
                ),
                ("patterns/x.svg", good.clone()),
            ],
            "needs TessaFab 99.0.0",
        ),
        (
            "badid",
            vec![
                (
                    "pack.json",
                    json!({"id": "../x", "name": "N", "version": "1"}).to_string(),
                ),
                ("patterns/x.svg", good.clone()),
            ],
            "pack id must be",
        ),
        (
            "nometa",
            vec![("patterns/x.svg", good.clone())],
            "has no pack.json",
        ),
    ];
    for (name, files, why) in cases {
        let path = dir.join(format!("{name}.tessafab-pack"));
        let refs: Vec<(&str, &str)> = files.iter().map(|(n, t)| (*n, t.as_str())).collect();
        zip_of(&path, &refs);
        let e = lib.import_pack(&path).unwrap_err().to_string();
        assert!(e.contains(why), "{name}: {e}");
        assert!(
            lib.packs().as_array().unwrap().is_empty(),
            "{name} installed something"
        );
    }
    // not a zip at all
    let path = dir.join("text.tessafab-pack");
    std::fs::write(&path, "hello").unwrap();
    assert!(lib
        .import_pack(&path)
        .unwrap_err()
        .to_string()
        .contains("not a zip"));
}
