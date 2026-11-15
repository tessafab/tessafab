//! Containers against real slicer files: the fixtures (PrusaSlicer .bgcode
//! and plain export of the same plate) and examples (Bambu Studio .gcode.3mf).
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use tessafab_core::bgcode::{self, BgcodeFile};
use tessafab_core::checks::state_mismatches;
use tessafab_core::container::{self, Kind};

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

fn prusa(name: &str) -> PathBuf {
    repo().join("fixtures/prusa").join(name)
}

fn example(name: &str) -> PathBuf {
    repo().join("examples").join(name)
}

/// G-code commands without comments or spacing differences.
fn commands(text: &[u8]) -> Vec<String> {
    String::from_utf8_lossy(text)
        .lines()
        .filter_map(|l| {
            let code: Vec<&str> = l.split(';').next().unwrap().split_whitespace().collect();
            (!code.is_empty()).then(|| code.join(" "))
        })
        .collect()
}

fn lines(data: &[u8]) -> Vec<String> {
    container::split_lines(data)
        .into_iter()
        .map(|l| String::from_utf8_lossy(l).into_owned())
        .collect()
}

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("pp-containers-{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d.join(name)
}

#[test]
fn bgcode_reads_the_same_commands_as_the_plain_export() {
    let f = BgcodeFile::parse(&std::fs::read(prusa("tiles_mini.bgcode")).unwrap()).unwrap();
    assert_eq!((f.version, f.checksum), (1, bgcode::CHECKSUM_CRC32));
    let blocks: Vec<_> = f.gcode_blocks().collect();
    assert!(!blocks.is_empty());
    assert!(blocks
        .iter()
        .all(|b| b.compression == bgcode::HEATSHRINK_12_4
            && b.encoding() == bgcode::MEATPACK_COMMENTS));
    let text = f.gcode().unwrap();
    let plain = std::fs::read(prusa("tiles_mini.gcode")).unwrap();
    assert_eq!(commands(&text), commands(&plain));
    assert!(String::from_utf8_lossy(&text).contains(";TYPE:Top solid infill"));
    assert_eq!(
        f.metadata(3)
            .unwrap()
            .get("printer_model")
            .map(String::as_str),
        Some("MINI")
    );
}

#[test]
fn bgcode_detects_corruption() {
    let mut data = std::fs::read(prusa("tiles_mini.bgcode")).unwrap();
    let mid = data.len() / 2;
    data[mid] ^= 0x55;
    let e = BgcodeFile::parse(&data).unwrap_err();
    assert!(e.0.contains("checksum"), "{}", e.0);
    assert_eq!(container::kind(&prusa("tiles_mini.gcode")), Kind::Gcode);
    assert_eq!(container::kind(&prusa("tiles_mini.bgcode")), Kind::Bgcode);
}

#[test]
fn bgcode_write_round_trip_keeps_metadata() {
    let f = BgcodeFile::parse(&std::fs::read(prusa("tiles_mini.bgcode")).unwrap()).unwrap();
    let text = f.gcode().unwrap();
    let out = f.with_gcode(&text).unwrap();
    let g = BgcodeFile::parse(&out).unwrap(); // checksums verified on parse
    let raw = |f: &BgcodeFile| -> Vec<Vec<u8>> {
        f.blocks
            .iter()
            .filter(|b| b.kind != bgcode::GCODE_BLOCK)
            .map(|b| b.raw.clone())
            .collect()
    };
    assert_eq!(raw(&g), raw(&f));
    assert!(g
        .gcode_blocks()
        .all(|b| b.compression == bgcode::HEATSHRINK_12_4
            && b.encoding() == bgcode::MEATPACK_COMMENTS));
    let back = g.gcode().unwrap();
    assert_eq!(commands(&back), commands(&text));
    assert!(state_mismatches(&lines(&text), &lines(&back), 1e-3, 5).is_empty());
}

#[test]
fn bgcode_is_one_plate_and_reads_through_the_container() {
    let p = prusa("tiles_mini.bgcode");
    assert_eq!(container::plates(&p).unwrap(), vec![1]);
    let text = container::read_plate(&p, 1).unwrap();
    assert!(String::from_utf8_lossy(&text).contains(";TYPE:Top solid infill"));
    let cfg = container::config_lines(&p, 1).unwrap();
    assert!(cfg.iter().any(|l| l.starts_with("; ") && l.contains(" = ")));
}

#[test]
fn threemf_plates_read_and_write_back() {
    let src = example("hexes.gcode.3mf");
    assert_eq!(container::kind(&src), Kind::ThreeMf);
    let plates = container::plates(&src).unwrap();
    assert_eq!(plates, vec![1, 2, 3]);
    assert!(container::md5_mismatches(&src).unwrap().is_empty());

    // replace plate 2 with an edited copy; plates 1 and 3 and everything else stay byte for byte
    let p2 = container::read_plate(&src, 2).unwrap();
    let mut edited = p2.clone();
    edited.extend_from_slice(b"; edited by the Rust engine test\n");
    let dst = tmp("hexes-out.gcode.3mf");
    let _ = std::fs::remove_file(&dst);
    container::write_3mf(&src, &dst, &BTreeMap::from([(2, edited.clone())]), None).unwrap();
    assert_eq!(container::plates(&dst).unwrap(), vec![1, 2, 3]);
    assert_eq!(container::read_plate(&dst, 2).unwrap(), edited);
    for n in [1, 3] {
        assert_eq!(
            container::read_plate(&dst, n).unwrap(),
            container::read_plate(&src, n).unwrap()
        );
    }
    assert!(
        container::md5_mismatches(&dst).unwrap().is_empty(),
        "fresh .md5 for the new plate"
    );

    let names = |p: &Path| -> Vec<String> {
        let z = zip::ZipArchive::new(std::fs::File::open(p).unwrap()).unwrap();
        z.file_names().map(|n| n.unwrap().to_string()).collect()
    };
    assert_eq!(names(&dst), names(&src), "same entries, same order");
    std::fs::remove_file(&dst).unwrap();
}

#[test]
fn threemf_edit_hook_changes_other_entries() {
    let src = example("tiles.gcode.3mf");
    let dst = tmp("tiles-edit.gcode.3mf");
    let _ = std::fs::remove_file(&dst);
    let mut seen = Vec::new();
    let mut edit = |name: &str, data: &[u8]| -> Option<Vec<u8>> {
        seen.push(name.to_string());
        (name == "Metadata/plate_1.json").then(|| {
            let mut d = data.to_vec();
            d.push(b' ');
            d
        })
    };
    container::write_3mf(&src, &dst, &BTreeMap::new(), Some(&mut edit)).unwrap();
    assert!(seen.iter().any(|n| n == "Metadata/plate_1.json"));
    let read = |p: &Path, name: &str| -> Vec<u8> {
        use std::io::Read;
        let mut z = zip::ZipArchive::new(std::fs::File::open(p).unwrap()).unwrap();
        let mut out = Vec::new();
        z.by_name(name).unwrap().read_to_end(&mut out).unwrap();
        out
    };
    let before = read(&src, "Metadata/plate_1.json");
    let after = read(&dst, "Metadata/plate_1.json");
    assert_eq!(after.len(), before.len() + 1);
    std::fs::remove_file(&dst).unwrap();
}

#[test]
fn plain_gcode_is_one_plate() {
    let p = prusa("tiles_mini.gcode");
    assert_eq!(container::plates(&p).unwrap(), vec![1]);
    assert_eq!(
        container::read_plate(&p, 1).unwrap(),
        std::fs::read(&p).unwrap()
    );
}
