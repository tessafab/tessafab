//! The slicer's settings, read from the G-code header: `; key = value` lines
//! between `; CONFIG_BLOCK_START` and `; CONFIG_BLOCK_END` (Bambu Studio,
//! OrcaSlicer), or every `; key = value` comment line (PrusaSlicer), typed
//! with Bambu Studio's own option types. A port of read_header_config and
//! parse_value in slicer_macro.py (the macro language comes with multi-colour).

use crate::bambu_types;
use std::collections::HashMap;

/// One setting's value.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Float(f64),
    Int(i64),
    Bool(bool),
    /// A percentage (the number before the `%`).
    Percent(f64),
    /// (value, is it a percentage).
    FloatOrPercent(f64, bool),
    Point(f64, f64),
    Str(String),
    /// Vectors (one entry per filament or extruder); None is `nil`.
    Floats(Vec<Option<f64>>),
    Ints(Vec<Option<i64>>),
    Bools(Vec<Option<bool>>),
    Percents(Vec<Option<f64>>),
    FloatsOrPercents(Vec<Option<(f64, bool)>>),
    Points(Vec<(f64, f64)>),
    Strs(Vec<String>),
    /// Anything exotic (points groups): the text as written.
    Raw(String),
}

impl Value {
    /// A number, from a scalar or the first element of a vector.
    pub fn as_f64(&self) -> Option<f64> {
        self.at_f64(0)
    }

    /// Element `idx` as a number, with Bambu's `get_at` rule (out of range: element 0).
    pub fn at_f64(&self, idx: usize) -> Option<f64> {
        fn pick<T: Copy>(v: &[Option<T>], idx: usize) -> Option<T> {
            if v.is_empty() {
                return None;
            }
            *v.get(idx).unwrap_or(&v[0])
        }
        match self {
            Value::Float(x) | Value::Percent(x) | Value::FloatOrPercent(x, _) => Some(*x),
            Value::Int(i) => Some(*i as f64),
            Value::Bool(b) => Some(*b as i64 as f64),
            Value::Floats(v) | Value::Percents(v) => pick(v, idx),
            Value::Ints(v) => pick(v, idx).map(|i| i as f64),
            Value::Bools(v) => pick(v, idx).map(|b| b as i64 as f64),
            Value::FloatsOrPercents(v) => pick(v, idx).map(|(x, _)| x),
            Value::Str(s) => s.trim().parse().ok(),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::Str(s) | Value::Raw(s) => Some(s),
            Value::Strs(v) => v.first().map(String::as_str),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ParseError;

fn float(s: &str) -> Result<f64, ParseError> {
    let t = s.trim();
    // Python's float() accepts inf / nan words; slicers don't write them
    t.parse::<f64>().map_err(|_| ParseError)
}

fn int(s: &str) -> Result<i64, ParseError> {
    Ok(float(s)?.trunc() as i64)
}

fn point(tok: &str) -> Result<(f64, f64), ParseError> {
    let (x, y) = tok.split_once('x').ok_or(ParseError)?;
    Ok((float(x)?, float(y)?))
}

fn truthy(s: &str) -> bool {
    matches!(s.trim().to_ascii_lowercase().as_str(), "1" | "true")
}

/// Undo `escape_string_cstyle` (used for `coString` values).
fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        if c == '\\' {
            if let Some(n) = it.next() {
                out.push(match n {
                    'n' => '\n',
                    'r' => '\r',
                    't' => '\t',
                    o => o,
                });
                continue;
            }
        }
        out.push(c);
    }
    out
}

/// Split a serialized `coStrings` value: `a;"b c";"d\"e"`.
fn split_strings(s: &str) -> Vec<String> {
    if s.is_empty() {
        return vec![String::new()];
    }
    let c: Vec<char> = s.chars().collect();
    let n = c.len();
    let mut out = Vec::new();
    let mut i = 0;
    loop {
        if i < n && c[i] == '"' {
            i += 1;
            let mut buf = String::new();
            while i < n && c[i] != '"' {
                if c[i] == '\\' && i + 1 < n {
                    i += 1;
                    buf.push(match c[i] {
                        'n' => '\n',
                        'r' => '\r',
                        't' => '\t',
                        o => o,
                    });
                } else {
                    buf.push(c[i]);
                }
                i += 1;
            }
            i += 1; // closing quote
            out.push(buf);
            while i < n && c[i] != ';' {
                i += 1;
            }
        } else {
            let j = (i..n).find(|&k| c[k] == ';').unwrap_or(n);
            out.push(c[i..j].iter().collect());
            i = j;
        }
        if i >= n {
            break;
        }
        i += 1; // ';'
        if i == n {
            out.push(String::new());
            break;
        }
    }
    out
}

fn is_number_list(raw: &str) -> bool {
    let num = |t: &str| {
        if t == "nil" {
            return true;
        }
        let t = t.strip_prefix('-').unwrap_or(t);
        let (mant, exp) = match t.split_once('e') {
            Some((m, e)) => (m, Some(e)),
            None => (t, None),
        };
        let (a, b) = match mant.split_once('.') {
            Some((a, b)) => (a, Some(b)),
            None => (mant, None),
        };
        let digits = |s: &str| !s.is_empty() && s.bytes().all(|c| c.is_ascii_digit());
        digits(a)
            && b.is_none_or(digits)
            && exp.is_none_or(|e| digits(e.strip_prefix('-').unwrap_or(e)))
    };
    raw.split(',').all(num)
}

/// The kind of a key Bambu Studio's table doesn't list (as the Python engine guesses it).
fn infer_kind(key: &str, raw: &str) -> &'static str {
    if let Some(rest) = key.strip_prefix("filament_") {
        if let Some(k) = bambu_types::kind_of(rest) {
            return k;
        }
    }
    if key.starts_with("machine_max_") {
        return "Floats";
    }
    if is_number_list(raw) {
        let parts: Vec<&str> = raw.split(',').collect();
        let has_dot = parts.iter().any(|p| p.contains('.') || p.contains('e'));
        return match (parts.len() == 1, has_dot) {
            (true, true) => "Float",
            (true, false) => "Int",
            (false, true) => "Floats",
            (false, false) => "Ints",
        };
    }
    if raw.contains(';') || raw.starts_with('"') {
        return "Strings";
    }
    "String"
}

fn vec_of<T>(
    raw: &str,
    f: impl Fn(&str) -> Result<T, ParseError>,
) -> Result<Vec<Option<T>>, ParseError> {
    if raw.is_empty() {
        return Ok(vec![]);
    }
    raw.split(',')
        .map(|t| {
            let t = t.trim();
            if t == "nil" {
                Ok(None)
            } else {
                f(t).map(Some)
            }
        })
        .collect()
}

/// Parse one serialized header value of the given Bambu option kind.
pub fn parse_value(kind: &str, raw: &str) -> Result<Value, ParseError> {
    let pct = |t: &str| float(t.trim_end_matches('%'));
    Ok(match kind {
        "Float" => Value::Float(float(raw)?),
        "Int" => Value::Int(int(raw)?),
        "Bool" => Value::Bool(truthy(raw)),
        "Percent" => Value::Percent(pct(raw)?),
        "FloatOrPercent" => Value::FloatOrPercent(pct(raw)?, raw.ends_with('%')),
        "Point" => {
            let (x, y) = point(raw)?;
            Value::Point(x, y)
        }
        "String" | "Enum" => Value::Str(unescape(raw)),
        "Floats" => Value::Floats(vec_of(raw, float)?),
        "Ints" => Value::Ints(vec_of(raw, int)?),
        "Bools" => Value::Bools(vec_of(raw, |t| Ok(truthy(t)))?),
        "Percents" => Value::Percents(vec_of(raw, pct)?),
        "FloatsOrPercents" => {
            Value::FloatsOrPercents(vec_of(raw, |t| Ok((pct(t)?, t.ends_with('%'))))?)
        }
        "Points" => Value::Points(if raw.is_empty() {
            vec![]
        } else {
            raw.split(',').map(point).collect::<Result<_, _>>()?
        }),
        "Strings" => Value::Strs(split_strings(raw)),
        "Enums" => Value::Strs(raw.split(',').map(String::from).collect()),
        _ => Value::Raw(raw.to_string()), // PointsGroups and anything exotic
    })
}

/// The header's settings: typed values, their kinds and the raw text.
#[derive(Debug, Clone, Default)]
pub struct SlicerConfig {
    pub values: HashMap<String, Value>,
    pub kinds: HashMap<String, String>,
    pub raw: HashMap<String, String>,
}

impl SlicerConfig {
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.values.get(key)
    }

    /// A number setting (first element of a vector).
    pub fn f64(&self, key: &str) -> Option<f64> {
        self.get(key).and_then(Value::as_f64)
    }

    /// Vector element `idx` with Bambu's `get_at` rule.
    pub fn at_f64(&self, key: &str, idx: usize) -> Option<f64> {
        self.get(key).and_then(|v| v.at_f64(idx))
    }
}

fn key_ok(key: &str) -> bool {
    !key.is_empty() && key.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_')
}

/// Typed settings from `; key = value` lines in the G-code's config block.
pub fn read_header_config<S: AsRef<str>>(lines: &[S]) -> SlicerConfig {
    let mut start = None;
    let mut end = None;
    for (i, ln) in lines.iter().enumerate() {
        let s = ln.as_ref().trim();
        if s == "; CONFIG_BLOCK_START" {
            start = Some(i + 1);
        } else if s == "; CONFIG_BLOCK_END" && start.is_some() {
            end = Some(i);
            break;
        }
    }
    let body = match (start, end) {
        (Some(a), Some(b)) => &lines[a..b],
        _ => lines,
    };
    let mut cfg = SlicerConfig::default();
    for ln in body {
        let ln = ln.as_ref();
        let Some(rest) = ln.strip_prefix("; ") else {
            continue;
        };
        let rest = rest.trim_end_matches(['\r', '\n']);
        let Some((key, raw)) = rest.split_once(" = ") else {
            continue;
        };
        let key = key.trim();
        if !key_ok(key) {
            continue;
        }
        let kind = bambu_types::kind_of(key).unwrap_or_else(|| infer_kind(key, raw));
        let (kind, value) = match parse_value(kind, raw) {
            Ok(v) => (kind, v),
            Err(_) => ("String", Value::Str(raw.to_string())),
        };
        cfg.raw.insert(key.to_string(), raw.to_string());
        cfg.kinds.insert(key.to_string(), kind.to_string());
        cfg.values.insert(key.to_string(), value);
    }
    cfg
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_kinds() {
        assert_eq!(parse_value("Float", "0.42").unwrap(), Value::Float(0.42));
        assert_eq!(parse_value("Int", "3.0").unwrap(), Value::Int(3));
        assert_eq!(parse_value("Percent", "15%").unwrap(), Value::Percent(15.0));
        assert_eq!(
            parse_value("FloatOrPercent", "50%").unwrap(),
            Value::FloatOrPercent(50.0, true)
        );
        assert_eq!(
            parse_value("Floats", "0.8,nil,1").unwrap(),
            Value::Floats(vec![Some(0.8), None, Some(1.0)])
        );
        assert_eq!(
            parse_value("Points", "0x0,256x256").unwrap(),
            Value::Points(vec![(0.0, 0.0), (256.0, 256.0)])
        );
        assert_eq!(
            parse_value("Strings", r#"PLA;"Bambu PLA \"Basic\"";;"#).unwrap(),
            Value::Strs(vec![
                "PLA".into(),
                "Bambu PLA \"Basic\"".into(),
                "".into(),
                "".into()
            ])
        );
        assert_eq!(
            parse_value("String", r"G28\nM104").unwrap(),
            Value::Str("G28\nM104".into())
        );
        assert!(parse_value("Float", "abc").is_err());
    }

    #[test]
    fn reads_the_config_block() {
        let lines = [
            "; HEADER\n",
            "; nozzle_diameter = 0.6\n",
            "; CONFIG_BLOCK_START\n",
            "; nozzle_diameter = 0.4\n",
            "; filament_type = PLA;PETG\n",
            "; retraction_length = 0.8,1.2\n",
            "; some_new_key = 3\n",
            "; bad key = 1\n",
            "; CONFIG_BLOCK_END\n",
        ];
        let c = read_header_config(&lines);
        assert_eq!(c.f64("nozzle_diameter"), Some(0.4));
        assert_eq!(
            c.get("filament_type"),
            Some(&Value::Strs(vec!["PLA".into(), "PETG".into()]))
        );
        assert_eq!(c.at_f64("retraction_length", 1), Some(1.2));
        assert_eq!(c.at_f64("retraction_length", 7), Some(0.8));
        assert_eq!(c.kinds["some_new_key"], "Int");
        assert!(!c.values.contains_key("bad key"));
        // no block: every "; key = value" line (PrusaSlicer)
        let p = read_header_config(&[
            "; nozzle_diameter = 0.4\n",
            "G1 X1\n",
            "; layer_height = 0.2\n",
        ]);
        assert_eq!(p.f64("layer_height"), Some(0.2));
    }
}
