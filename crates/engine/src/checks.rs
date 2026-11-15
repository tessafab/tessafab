//! Checks run on every result before export (and by the tests).
//!
//! Rewritten G-code must leave the printer exactly where the original did:
//! the same X/Y/Z/F (and E, when it is absolute) at every layer change and at
//! the end. A port of checks.py.

use crate::gcode::{is_layer_change, parse_line, Machine};

/// Printer state at a layer change: x, y, z, f, and e when extrusion is absolute.
pub type State = (f64, f64, f64, f64, Option<f64>);

fn state(m: &Machine) -> State {
    (m.x, m.y, m.z, m.f, if m.abs_e { Some(m.e) } else { None })
}

/// The printer state at every layer change, plus the final state.
pub fn layer_states<S: AsRef<str>>(lines: &[S]) -> Vec<State> {
    let mut m = Machine::new();
    let mut out = Vec::new();
    for line in lines {
        let line = line.as_ref();
        let s = line.trim();
        if s.starts_with(';') {
            if is_layer_change(s) {
                out.push(state(&m));
            }
            continue;
        }
        if let Some((cmd, p)) = parse_line(line) {
            m.apply(&cmd, &p);
        }
    }
    out.push(state(&m));
    out
}

/// One place where the processed G-code's state differs from the original's.
#[derive(Debug, Clone, PartialEq)]
pub enum Mismatch {
    /// The two have a different number of layer changes.
    Layers { want: usize, got: usize },
    /// At layer change `index` (the last index is the end of the file), `axis` differs.
    Axis {
        index: usize,
        axis: char,
        want: Option<f64>,
        got: Option<f64>,
    },
}

/// Where `processed` leaves the printer somewhere other than `original` did
/// (at most `limit` places; tolerance `tol` mm).
pub fn state_mismatches<S: AsRef<str>, T: AsRef<str>>(
    original: &[S],
    processed: &[T],
    tol: f64,
    limit: usize,
) -> Vec<Mismatch> {
    state_mismatches_raised(original, processed, tol, limit, 0)
}

/// [`state_mismatches`] for a print whose raised tiles (relief, M22b) added
/// `added` layers after the original last one: every original layer change
/// must match; the end must too, but higher (Z at least the original's).
pub fn state_mismatches_raised<S: AsRef<str>, T: AsRef<str>>(
    original: &[S],
    processed: &[T],
    tol: f64,
    limit: usize,
    added: usize,
) -> Vec<Mismatch> {
    let a = layer_states(original);
    let mut b = layer_states(processed);
    if added > 0 && b.len() == a.len() + added {
        // the added layer changes sit just before the end: drop them, and
        // let the end be higher
        let end = b.pop().unwrap();
        b.truncate(a.len() - 1);
        let want = a[a.len() - 1];
        b.push(if end.2 >= want.2 - tol {
            (end.0, end.1, want.2, end.3, end.4)
        } else {
            end
        });
    }
    if a.len() != b.len() {
        return vec![Mismatch::Layers {
            want: a.len(),
            got: b.len(),
        }];
    }
    let mut bad = Vec::new();
    for (k, (sa, sb)) in a.iter().zip(&b).enumerate() {
        let pairs = [
            ('X', Some(sa.0), Some(sb.0)),
            ('Y', Some(sa.1), Some(sb.1)),
            ('Z', Some(sa.2), Some(sb.2)),
            ('F', Some(sa.3), Some(sb.3)),
            ('E', sa.4, sb.4),
        ];
        for (axis, va, vb) in pairs {
            let differs = match (va, vb) {
                (Some(x), Some(y)) => (x - y).abs() > tol,
                (None, None) => false,
                _ => true,
            };
            if differs {
                bad.push(Mismatch::Axis {
                    index: k,
                    axis,
                    want: va,
                    got: vb,
                });
                break;
            }
        }
        if bad.len() >= limit {
            break;
        }
    }
    bad
}

/// Top surfaces left as sliced, from a rewrite log: `(layer, reason)`.
pub fn kept_surfaces(log: &str) -> Vec<(usize, String)> {
    const TOP: &str = ": kept original top surface (";
    const BOTTOM: &str = ": kept original bottom surface (";
    log.lines()
        .filter_map(|line| {
            let line = line.trim();
            let rest = line.strip_prefix("layer ")?;
            let (num, reason) = match rest.split_once(TOP) {
                Some((num, tail)) => (num, tail.strip_suffix(')')?.to_string()),
                None => {
                    let (num, tail) = rest.split_once(BOTTOM)?;
                    (num, format!("{} (bottom surface)", tail.strip_suffix(')')?))
                }
            };
            Some((num.parse().ok()?, reason))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_moves_same_state() {
        let a = [
            "G90\n",
            "M82\n",
            "; CHANGE_LAYER\n",
            "G1 X1 Y2 Z0.2 E1 F600\n",
            ";LAYER_CHANGE\n",
            "G1 X5 E2\n",
        ];
        assert!(state_mismatches(&a, &a, 1e-3, 5).is_empty());
        assert_eq!(layer_states(&a).len(), 3);
    }

    #[test]
    fn a_different_end_point_is_found() {
        let a = ["; CHANGE_LAYER\n", "G1 X1 Y2 F600\n", "; CHANGE_LAYER\n"];
        let b = ["; CHANGE_LAYER\n", "G1 X1 Y2.5 F600\n", "; CHANGE_LAYER\n"];
        assert_eq!(
            state_mismatches(&a, &b, 1e-3, 5),
            vec![
                Mismatch::Axis {
                    index: 1,
                    axis: 'Y',
                    want: Some(2.0),
                    got: Some(2.5)
                },
                Mismatch::Axis {
                    index: 2,
                    axis: 'Y',
                    want: Some(2.0),
                    got: Some(2.5)
                }
            ]
        );
        let c = ["; CHANGE_LAYER\n"];
        assert_eq!(
            state_mismatches(&a, &c, 1e-3, 5),
            vec![Mismatch::Layers { want: 3, got: 2 }]
        );
    }

    #[test]
    fn relative_extrusion_is_not_compared() {
        let a = ["M83\n", "G1 X1 E1\n"];
        let b = ["M83\n", "G1 X1 E2\n"];
        assert!(state_mismatches(&a, &b, 1e-3, 5).is_empty());
    }

    #[test]
    fn reads_kept_surfaces_from_a_log() {
        let log = "layer 22: patterned 13 block(s)\nlayer 39: kept original top surface (above --max-z)\n";
        assert_eq!(kept_surfaces(log), vec![(39, "above --max-z".to_string())]);
    }
}
