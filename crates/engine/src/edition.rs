//! Editions (ADR 0004, ADR 0007): what a session may do.
//!
//! The core alone (a build from the Community source) opens one plate with
//! one piece. The Pro plugin sets the edition from the licence: without one
//! (the official Community app) anything opens and the whole assembly can be
//! laid out, but only one chosen piece (the first placement) is patterned;
//! the trial and Pro pattern everything, and trial exports are marked as not
//! for commercial use.

use std::borrow::Cow;

use serde_json::{json, Value};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Edition {
    /// Built from the Community source: one plate, one piece.
    Source,
    /// The official app without a licence: one chosen piece is patterned.
    Community,
    /// Pro features on trial: exports carry [`TRIAL_MARK`].
    Trial,
    Pro,
}

/// The G-code header line of a trial export.
pub const TRIAL_MARK: &str = "; TessaFab trial - not for commercial use\n";

const UPGRADE: &str = "TessaFab Pro (or its 14-day trial)";

impl Edition {
    pub fn name(self) -> &'static str {
        match self {
            Edition::Source => "source",
            Edition::Community => "community",
            Edition::Trial => "trial",
            Edition::Pro => "pro",
        }
    }

    /// Patterns every piece on every plate (trial or Pro).
    pub fn full(self) -> bool {
        matches!(self, Edition::Trial | Edition::Pro)
    }

    /// Whether a Pro feature (`what`, e.g. "Layered SVG export") may be used.
    pub fn check_pro(self, what: &str) -> Result<(), String> {
        if self.full() {
            Ok(())
        } else {
            Err(format!("{what} is part of {UPGRADE}"))
        }
    }

    /// For the app: the edition and its limits.
    pub fn describe(self) -> Value {
        json!({
            "edition": self.name(),
            "maxPlates": if self == Edition::Source { json!(1) } else { Value::Null },
            "maxPieces": if self.full() { Value::Null } else { json!(1) },
        })
    }

    /// Whether files with `plates` plates holding `pieces` pieces in all may be opened.
    pub fn check_open(self, plates: usize, pieces: usize) -> Result<(), String> {
        if self != Edition::Source || (plates <= 1 && pieces <= 1) {
            return Ok(());
        }
        let what = if plates > 1 {
            format!("{plates} plates")
        } else {
            format!("{pieces} pieces")
        };
        Err(format!(
            "these files hold {what}; TessaFab built from the Community source patterns \
             one piece on one plate. Slice the piece on its own plate, or use the official \
             TessaFab app, which patterns one piece of a bigger plate for free"
        ))
    }

    /// How many of the assembly's placements this edition patterns: every one
    /// with Pro (or its trial), else only the first (the piece the user
    /// chose). The rest stay in the project, shown but left as sliced.
    pub fn patterned_count(self, placed: usize) -> usize {
        if self.full() {
            placed
        } else {
            placed.min(1)
        }
    }

    /// The project as this edition patterns it: its placements cut to
    /// [`Edition::patterned_count`].
    pub fn patterned(self, project: &Value) -> Cow<'_, Value> {
        let placed = project["assembly"]["placements"]
            .as_array()
            .map_or(0, Vec::len);
        let n = self.patterned_count(placed);
        if n == placed {
            return Cow::Borrowed(project);
        }
        let mut p = project.clone();
        if let Some(pls) = p["assembly"]["placements"].as_array_mut() {
            pls.truncate(n);
        }
        Cow::Owned(p)
    }

    /// Whether an assembly of `placed` pieces may be patterned.
    pub fn check_pattern(self, placed: usize) -> Result<(), String> {
        if self.full() || placed <= 1 {
            return Ok(());
        }
        Err(format!(
            "patterning {placed} pieces together needs {UPGRADE}; without it, choose one piece to pattern"
        ))
    }
}

/// Whether `line` is the header note the engine writes into its output
/// (also as written before the app was renamed TessaFab).
pub fn is_engine_note(line: &str) -> bool {
    line.starts_with("; postprocessed by TessaFab")
        || line.starts_with("; postprocessed by herringbone.py")
}

/// Puts [`TRIAL_MARK`] after the engine's own header note (or first).
pub fn mark_trial(lines: &mut Vec<String>) {
    if lines.iter().take(1000).any(|l| l == TRIAL_MARK) {
        return;
    }
    let at = lines
        .iter()
        .take(1000)
        .position(|l| is_engine_note(l))
        .map_or(0, |i| i + 1);
    lines.insert(at, TRIAL_MARK.to_string());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limits_by_edition() {
        assert!(Edition::Source.check_open(1, 1).is_ok());
        assert!(Edition::Source
            .check_open(2, 2)
            .unwrap_err()
            .contains("2 plates"));
        assert!(Edition::Source
            .check_open(1, 6)
            .unwrap_err()
            .contains("6 pieces"));
        assert!(Edition::Community.check_open(2, 8).is_ok());
        assert!(Edition::Community.check_pattern(1).is_ok());
        assert!(Edition::Community
            .check_pattern(2)
            .unwrap_err()
            .contains("choose one piece"));
        assert!(Edition::Trial.check_pattern(8).is_ok());
        assert!(Edition::Pro.check_pattern(8).is_ok());
    }

    #[test]
    fn without_pro_only_the_first_placement_is_patterned() {
        let project =
            json!({"assembly": {"placements": [{"piece": 3}, {"piece": 1}, {"piece": 2}]}});
        let free = Edition::Community.patterned(&project);
        assert_eq!(free["assembly"]["placements"], json!([{"piece": 3}]));
        assert!(matches!(Edition::Pro.patterned(&project), Cow::Borrowed(_)));
        assert_eq!(Edition::Trial.patterned_count(3), 3);
        assert_eq!(Edition::Community.patterned_count(0), 0);
    }

    #[test]
    fn the_trial_mark_follows_the_engine_note_once() {
        let mut lines = vec![
            "; HEADER_BLOCK_START\n".to_string(),
            "; postprocessed by TessaFab 1.2.0: 3 top surface block(s) rewritten, 0 kept\n"
                .to_string(),
            "G28\n".to_string(),
        ];
        mark_trial(&mut lines);
        mark_trial(&mut lines);
        assert_eq!(lines[2], TRIAL_MARK);
        assert_eq!(lines.iter().filter(|l| *l == TRIAL_MARK).count(), 1);
    }

    #[test]
    fn engine_notes_old_and_new() {
        assert!(is_engine_note(
            "; postprocessed by TessaFab 1.2.0: 3 top surface block(s) rewritten, 0 kept\n"
        ));
        assert!(is_engine_note(
            "; postprocessed by herringbone.py 1.2.0: 3 top surface block(s) rewritten, 0 kept\n"
        ));
        assert!(!is_engine_note("; generated by BambuStudio\n"));
    }
}
