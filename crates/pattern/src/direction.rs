//! Line direction: the way the lines in a tile run.
//!
//! Every target handles each kind of direction itself, so code that acts on a
//! direction matches on it exhaustively (no `_ =>` arm): adding a kind, such as
//! a direction field taken from an image, then makes the compiler list every
//! place that must handle it (ADR 0008).

use serde_json::Value;

/// The way the lines in a tile run.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Direction {
    /// Parallel lines at this angle, in degrees counter-clockwise from +x.
    Angle(f64),
    /// Concentric rings following the tile's outline.
    Concentric,
}

impl Direction {
    /// From the older pair of fields (a pattern dumped by the Python engine):
    /// rings win over an angle, and no angle means 0 degrees.
    pub fn from_parts(angle: Option<f64>, concentric: bool) -> Direction {
        if concentric {
            Direction::Concentric
        } else {
            Direction::Angle(angle.unwrap_or(0.0))
        }
    }

    /// As pattern, preset and project files write it: degrees, or `"rings"`.
    pub fn from_json(v: &Value) -> Option<Direction> {
        match v {
            Value::String(s) if s == "rings" => Some(Direction::Concentric),
            Value::Number(n) => n.as_f64().map(Direction::Angle),
            _ => None,
        }
    }

    /// Degrees, or `"rings"` (see [`Direction::from_json`]).
    pub fn to_json(&self) -> Value {
        match self {
            Direction::Concentric => Value::String("rings".into()),
            Direction::Angle(a) => serde_json::Number::from_f64(*a)
                .map(Value::Number)
                .unwrap_or(Value::Null),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn reads_and_writes_files_as_before() {
        assert_eq!(
            Direction::from_json(&json!(45)),
            Some(Direction::Angle(45.0))
        );
        assert_eq!(
            Direction::from_json(&json!("rings")),
            Some(Direction::Concentric)
        );
        assert_eq!(Direction::from_json(&json!("diagonal")), None);
        assert_eq!(Direction::from_json(&json!(true)), None);
        assert_eq!(Direction::Angle(30.0).to_json(), json!(30.0));
        assert_eq!(Direction::Concentric.to_json(), json!("rings"));
    }

    #[test]
    fn rings_win_over_an_angle() {
        assert_eq!(
            Direction::from_parts(Some(10.0), true),
            Direction::Concentric
        );
        assert_eq!(Direction::from_parts(None, false), Direction::Angle(0.0));
    }
}
