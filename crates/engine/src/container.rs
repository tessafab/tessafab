//! The files sliced G-code comes in: a Bambu Studio / OrcaSlicer project
//! (.gcode.3mf, one G-code file per plate), Prusa binary G-code (.bgcode) or
//! plain G-code text. A port of gcode3mf.py.
//!
//! A .gcode.3mf is a zip file; each plate's G-code is `Metadata/plate_N.gcode`
//! with its MD5 (upper-case hex) in `Metadata/plate_N.gcode.md5`. Plain and
//! binary G-code are a single plate, number 1.
//!
//! G-code is handled as bytes: text in UTF-8 (slicers' comments may hold any
//! language), except binary G-code, whose text is Latin-1. Lines are passed
//! through unchanged, so nothing is lost either way.

use crate::bgcode::{self, BgcodeFile};
use md5::{Digest, Md5};
use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::Path;

#[derive(Debug)]
pub struct Error(pub String);

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error(e.to_string())
    }
}

impl From<zip::result::ZipError> for Error {
    fn from(e: zip::result::ZipError) -> Self {
        Error(format!("not a readable .3mf: {e}"))
    }
}

impl From<bgcode::Error> for Error {
    fn from(e: bgcode::Error) -> Self {
        Error(e.0)
    }
}

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// A Bambu Studio / OrcaSlicer sliced project.
    ThreeMf,
    /// Prusa binary G-code.
    Bgcode,
    /// Plain G-code text.
    Gcode,
}

impl Kind {
    pub fn name(self) -> &'static str {
        match self {
            Kind::ThreeMf => "3mf",
            Kind::Bgcode => "bgcode",
            Kind::Gcode => "gcode",
        }
    }
}

fn is_3mf(path: &Path) -> bool {
    path.to_string_lossy().to_lowercase().ends_with(".3mf")
}

pub fn kind(path: &Path) -> Kind {
    if is_3mf(path) {
        return Kind::ThreeMf;
    }
    let mut head = [0u8; 4];
    let is_bin = File::open(path)
        .and_then(|mut f| f.read_exact(&mut head))
        .is_ok()
        && bgcode::is_bgcode(&head);
    if is_bin {
        Kind::Bgcode
    } else {
        Kind::Gcode
    }
}

fn plate_number(name: &str) -> Option<u32> {
    name.strip_prefix("Metadata/plate_")?
        .strip_suffix(".gcode")?
        .parse()
        .ok()
}

fn md5_plate_number(name: &str) -> Option<u32> {
    name.strip_prefix("Metadata/plate_")?
        .strip_suffix(".gcode.md5")?
        .parse()
        .ok()
}

fn open_zip(path: &Path) -> Result<zip::ZipArchive<File>> {
    Ok(zip::ZipArchive::new(File::open(path)?)?)
}

/// Plate numbers in the file, in order (`[1]` for plain or binary G-code).
pub fn plates(path: &Path) -> Result<Vec<u32>> {
    if kind(path) != Kind::ThreeMf {
        return Ok(vec![1]);
    }
    let z = open_zip(path)?;
    let mut out: Vec<u32> = z
        .file_names()
        .filter_map(|n| n.ok().and_then(|n| plate_number(&n)))
        .collect();
    out.sort_unstable();
    Ok(out)
}

/// The plate's G-code text, as bytes.
pub fn read_plate(path: &Path, n: u32) -> Result<Vec<u8>> {
    match kind(path) {
        Kind::Bgcode => Ok(BgcodeFile::parse(&fs::read(path)?)?.gcode()?),
        Kind::Gcode => Ok(fs::read(path)?),
        Kind::ThreeMf => {
            let mut z = open_zip(path)?;
            let mut f = z
                .by_name(&format!("Metadata/plate_{n}.gcode"))
                .map_err(|_| Error(format!("{} has no plate {n}", path.display())))?;
            let mut data = Vec::with_capacity(f.size() as usize);
            f.read_to_end(&mut data)?;
            Ok(data)
        }
    }
}

/// G-code bytes split into lines, each keeping its line ending.
pub fn split_lines(data: &[u8]) -> Vec<&[u8]> {
    let mut out = Vec::with_capacity(data.len() / 24);
    let mut start = 0;
    for (i, &c) in data.iter().enumerate() {
        if c == b'\n' {
            out.push(&data[start..=i]);
            start = i + 1;
        }
    }
    if start < data.len() {
        out.push(&data[start..]);
    }
    out
}

/// Upper-case hex MD5, as Bambu Studio writes it next to each plate.
pub fn md5_upper(data: &[u8]) -> String {
    Md5::digest(data)
        .iter()
        .map(|b| format!("{b:02X}"))
        .collect()
}

/// Copy the project `src` to `dst`, replacing plates `{n: G-code}`.
///
/// Every other entry is copied unchanged, compressed bytes and all, in the same
/// order, except that `edit(name, data)` may return new contents for it; a
/// replaced plate gets a fresh `.md5`. Written to a temporary file first.
pub fn write_3mf(
    src: &Path,
    dst: &Path,
    new_plates: &BTreeMap<u32, Vec<u8>>,
    mut edit: Option<&mut dyn FnMut(&str, &[u8]) -> Option<Vec<u8>>>,
) -> Result<()> {
    let tmp = dst.with_extension("tmp");
    {
        let mut zin = open_zip(src)?;
        let mut zout = zip::ZipWriter::new(File::create(&tmp)?);
        for i in 0..zin.len() {
            let (name, method, mtime, perms) = {
                let f = zin.by_index_raw(i)?;
                (
                    f.name()?.to_string(),
                    f.compression(),
                    f.last_modified(),
                    f.unix_mode(),
                )
            };
            let replaced: Option<Vec<u8>> = if let Some(n) = plate_number(&name) {
                new_plates.get(&n).cloned()
            } else if let Some(n) = md5_plate_number(&name) {
                new_plates.get(&n).map(|g| md5_upper(g).into_bytes())
            } else if let Some(edit) = edit.as_mut() {
                let mut data = Vec::new();
                zin.by_index(i)?.read_to_end(&mut data)?;
                edit(&name, &data)
            } else {
                None
            };
            match replaced {
                None => zout.raw_copy_file(zin.by_index_raw(i)?)?,
                Some(data) => {
                    let mut opts =
                        zip::write::SimpleFileOptions::default().compression_method(method);
                    if let Some(t) = mtime {
                        opts = opts.last_modified_time(t);
                    }
                    if let Some(m) = perms {
                        opts = opts.unix_permissions(m);
                    }
                    if data.len() >= 0xFFFF_FFFF {
                        opts = opts.large_file(true);
                    }
                    zout.start_file(name, opts)?;
                    zout.write_all(&data)?;
                }
            }
        }
        zout.finish()?;
    }
    fs::rename(&tmp, dst)?;
    Ok(())
}

/// Write G-code `text` to `dst` in the container of `src`: a copy of the
/// .3mf with plate `n` replaced, the .bgcode re-encoded, or plain text.
pub fn write_plate(src: &Path, dst: &Path, n: u32, text: &[u8]) -> Result<()> {
    match kind(src) {
        Kind::ThreeMf => write_3mf(src, dst, &BTreeMap::from([(n, text.to_vec())]), None),
        Kind::Bgcode => {
            let orig = BgcodeFile::parse(&fs::read(src)?)?;
            write_atomic(dst, &orig.with_gcode(text)?)
        }
        Kind::Gcode => write_atomic(dst, text),
    }
}

pub fn write_atomic(dst: &Path, data: &[u8]) -> Result<()> {
    let tmp = dst.with_extension("tmp");
    fs::write(&tmp, data)?;
    fs::rename(&tmp, dst)?;
    Ok(())
}

/// The plate's slicer settings as `; key = value` lines. Text G-code carries
/// them in comments; binary G-code keeps them in its slicer-metadata block.
pub fn config_lines(path: &Path, n: u32) -> Result<Vec<String>> {
    if kind(path) == Kind::Bgcode {
        let f = BgcodeFile::parse(&fs::read(path)?)?;
        return Ok(f
            .metadata_lines(2)?
            .into_iter()
            .map(|(k, v)| format!("; {k} = {v}\n"))
            .collect());
    }
    let data = read_plate(path, n)?;
    Ok(split_lines(&data)
        .into_iter()
        .map(|l| String::from_utf8_lossy(l).into_owned())
        .collect())
}

/// Plates of a .gcode.3mf whose `.md5` entry doesn't match their G-code.
pub fn md5_mismatches(path: &Path) -> Result<Vec<u32>> {
    let mut bad = Vec::new();
    let mut z = open_zip(path)?;
    for n in plates(path)? {
        let mut data = Vec::new();
        z.by_name(&format!("Metadata/plate_{n}.gcode"))?
            .read_to_end(&mut data)?;
        let mut want = String::new();
        z.by_name(&format!("Metadata/plate_{n}.gcode.md5"))?
            .read_to_string(&mut want)?;
        if md5_upper(&data) != want.trim() {
            bad.push(n);
        }
    }
    Ok(bad)
}
