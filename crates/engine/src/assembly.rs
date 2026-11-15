//! The assembly, as the G-code engine uses it: the format-neutral part
//! (placements, re-linking, strip fit, layout SVG, the patterned assembly of
//! one plate) is `tessafab_pattern::assembly`, re-exported here; this module
//! adds finding which plate of a project a lone G-code file is.

pub use tessafab_pattern::assembly::*;

use geo::Polygon;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

fn err<T>(m: impl Into<String>) -> Result<T> {
    Err(Error(m.into()))
}

/// Which (input, plate) of the project a lone plate is (slicer hook), and the
/// links. An error if no plate, or more than one, has every one of its
/// placements found among `pieces`.
pub fn pick_plate(placements: &[Value], pieces: &[Polygon<f64>]) -> Result<(PlateKey, Vec<Link>)> {
    let keys: BTreeSet<PlateKey> = placements.iter().map(key_of).collect();
    let mut fits: Vec<(usize, PlateKey, Vec<Link>)> = Vec::new();
    for key in keys {
        let plates = BTreeMap::from([(key, pieces.to_vec())]);
        let (links, _) = relink(placements, &plates);
        let mine: Vec<&Link> = placements
            .iter()
            .zip(&links)
            .filter(|(p, _)| key_of(p) == key)
            .map(|(_, l)| l)
            .collect();
        if mine.iter().all(|l| l.placed()) {
            fits.push((mine.iter().filter(|l| l.status == "ok").count(), key, links));
        }
    }
    if fits.is_empty() {
        return err("this G-code's pieces don't match any plate in the project");
    }
    fits.sort_by_key(|f| std::cmp::Reverse(f.0));
    if fits.len() > 1 && fits[0].0 == fits[1].0 {
        let names: Vec<String> = fits.iter().map(|f| format!("plate {}", f.1 .1)).collect();
        return err(format!(
            "this G-code matches several plates of the project ({}); use the app to generate it",
            names.join(", ")
        ));
    }
    let (_, key, links) = fits.swap_remove(0);
    Ok((key, links))
}
