//! The parts of Form.io form JSON the engine needs: a pattern's parameters.
//!
//! A pattern's parameter form is a Form.io form (`{"components": [...]}`). The
//! engine only reads its input components (number, select, radio, checkbox,
//! textfield, hidden, slider; nested in panels, columns, tables ...) to know
//! each parameter's key, default and limits, and checks values against them.
//! How the form looks is up to the app, which renders it with @formio/js.
//! A port of formio.py.

use serde_json::{Map, Value};
use std::fmt;

pub const INPUT_TYPES: [&str; 7] = [
    "number",
    "select",
    "radio",
    "checkbox",
    "textfield",
    "hidden",
    "slider",
];

#[derive(Debug, Clone, PartialEq)]
pub struct Error(pub String);

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Error {}

/// Python truthiness of a JSON value (`x or default` in the Python engine).
pub fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64() != Some(0.0),
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(o)) => !o.is_empty(),
    }
}

fn str_of<'a>(c: &'a Map<String, Value>, k: &str) -> Option<&'a str> {
    c.get(k).and_then(Value::as_str)
}

/// Every input component of the form, in order (descending into layout components).
pub fn inputs(form: &Value) -> Vec<&Map<String, Value>> {
    fn walk<'a>(components: Option<&'a Value>, out: &mut Vec<&'a Map<String, Value>>) {
        let Some(Value::Array(list)) = components else {
            return;
        };
        for c in list {
            let Value::Object(c) = c else { continue };
            if truthy(c.get("input"))
                && str_of(c, "key").is_some_and(|k| !k.is_empty())
                && str_of(c, "type").is_some_and(|t| INPUT_TYPES.contains(&t))
            {
                out.push(c);
            }
            walk(c.get("components"), out);
            if let Some(Value::Array(cols)) = c.get("columns") {
                for col in cols {
                    walk(col.get("components"), out);
                }
            }
            if let Some(Value::Array(rows)) = c.get("rows") {
                for row in rows {
                    if let Value::Array(cells) = row {
                        for cell in cells {
                            walk(cell.get("components"), out);
                        }
                    }
                }
            }
        }
    }
    let mut out = Vec::new();
    walk(form.get("components"), &mut out);
    out
}

fn key(c: &Map<String, Value>) -> &str {
    str_of(c, "key").unwrap_or("")
}

fn kind(c: &Map<String, Value>) -> &str {
    str_of(c, "type").unwrap_or("")
}

fn options(c: &Map<String, Value>) -> Vec<Value> {
    let list = if kind(c) == "radio" {
        c.get("values")
    } else {
        c.get("data").and_then(|d| d.get("values"))
    };
    match list {
        Some(Value::Array(vs)) => vs
            .iter()
            .map(|v| v.get("value").cloned().unwrap_or(Value::Null))
            .collect(),
        _ => Vec::new(),
    }
}

fn limit(c: &Map<String, Value>, name: &str) -> Option<f64> {
    match c.get("validate").and_then(|v| v.get(name)) {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) if s.is_empty() => None,
        Some(Value::String(s)) => s.trim().parse().ok(),
        Some(v) => v.as_f64(),
    }
}

/// A component's default value.
pub fn default(c: &Map<String, Value>) -> Value {
    match c.get("defaultValue") {
        Some(Value::Null) | None => {}
        Some(Value::String(s)) if s.is_empty() => {}
        Some(v) => return v.clone(),
    }
    match kind(c) {
        "checkbox" => Value::Bool(false),
        "select" | "radio" => options(c)
            .into_iter()
            .next()
            .unwrap_or(Value::String(String::new())),
        "number" | "slider" => c
            .get("validate")
            .and_then(|v| v.get("min"))
            .filter(|v| !v.is_null())
            .cloned()
            .unwrap_or(Value::from(0)),
        _ => Value::String(String::new()),
    }
}

/// Every parameter's default, in form order.
pub fn defaults(form: &Value) -> Map<String, Value> {
    inputs(form)
        .into_iter()
        .map(|c| (key(c).to_string(), default(c)))
        .collect()
}

/// JSON values equal as Python compares them (1 == 1.0, true == 1).
fn loosely_equal(a: &Value, b: &Value) -> bool {
    let num = |v: &Value| match v {
        Value::Number(n) => n.as_f64(),
        Value::Bool(b) => Some(*b as i64 as f64),
        _ => None,
    };
    match (num(a), num(b)) {
        (Some(x), Some(y)) => x == y,
        _ => a == b,
    }
}

fn fmt_g(x: f64) -> String {
    // Python's %g for the limits in messages: integers without a point
    if x == x.trunc() && x.abs() < 1e15 {
        format!("{}", x as i64)
    } else {
        format!("{x}")
    }
}

fn round_half_even(x: f64) -> f64 {
    if (x - x.trunc()).abs() == 0.5 {
        2.0 * (x / 2.0).round()
    } else {
        x.round()
    }
}

/// Values for every parameter of the form: given ones checked, missing ones defaulted.
pub fn check(
    form: &Value,
    values: Option<&Value>,
    what: &str,
) -> Result<Map<String, Value>, Error> {
    let empty = Map::new();
    // values or {}
    let values = match values {
        v if !truthy(v) => &empty,
        None => &empty,
        Some(Value::Object(m)) => m,
        Some(_) => return Err(Error(format!("{what} values must be an object"))),
    };
    let mut out = Map::new();
    for c in inputs(form) {
        let k = key(c);
        let label = str_of(c, "label").filter(|l| !l.is_empty()).unwrap_or(k);
        let mut v = values.get(k).cloned().unwrap_or_else(|| default(c));
        match kind(c) {
            "number" | "slider" => {
                let x = match &v {
                    Value::Number(n) => n.as_f64().unwrap_or(f64::NAN),
                    Value::String(s) => match s.trim().parse::<f64>() {
                        Ok(x) => x,
                        Err(_) => return Err(Error(format!("{label} must be a number"))),
                    },
                    _ => return Err(Error(format!("{label} must be a number"))),
                };
                let (lo, hi) = (limit(c, "min"), limit(c, "max"));
                if lo.is_some_and(|lo| x < lo - 1e-9) || hi.is_some_and(|hi| x > hi + 1e-9) {
                    return Err(Error(match (lo, hi) {
                        (Some(lo), Some(hi)) => {
                            format!("{label} must be between {} and {}", fmt_g(lo), fmt_g(hi))
                        }
                        (Some(lo), None) => format!("{label} must be at least {}", fmt_g(lo)),
                        (None, hi) => {
                            format!("{label} must be at most {}", fmt_g(hi.unwrap_or(0.0)))
                        }
                    }));
                }
                let integer = truthy(c.get("validate").and_then(|v| v.get("integer")))
                    || c.get("decimalLimit").and_then(Value::as_f64) == Some(0.0);
                v = if integer {
                    Value::from(round_half_even(x) as i64)
                } else {
                    serde_json::Number::from_f64(x)
                        .map(Value::Number)
                        .unwrap_or(Value::Null)
                };
            }
            "checkbox" => v = Value::Bool(truthy(Some(&v))),
            "select" | "radio" => {
                let opts = options(c);
                if !opts.is_empty() && !opts.iter().any(|o| loosely_equal(o, &v)) {
                    let names: Vec<String> = opts
                        .iter()
                        .map(|o| match o {
                            Value::String(s) => s.clone(),
                            o => o.to_string(),
                        })
                        .collect();
                    return Err(Error(format!(
                        "{label} must be one of {}",
                        names.join(", ")
                    )));
                }
            }
            _ => {}
        }
        out.insert(k.to_string(), v);
    }
    Ok(out)
}

/// An error unless `form` is a usable parameter form.
pub fn validate_form(form: &Value) -> Result<(), Error> {
    let ok = form.is_object() && matches!(form.get("components"), None | Some(Value::Array(_)));
    if !ok {
        return Err(Error(
            "the parameter form must be Form.io JSON: {\"components\": [...]}".into(),
        ));
    }
    let keys: Vec<&str> = inputs(form).into_iter().map(key).collect();
    let mut dup: Vec<&str> = keys
        .iter()
        .copied()
        .filter(|k| keys.iter().filter(|x| *x == k).count() > 1)
        .collect();
    dup.sort_unstable();
    dup.dedup();
    if !dup.is_empty() {
        return Err(Error(format!(
            "parameter name used twice: {}",
            dup.join(", ")
        )));
    }
    for k in keys {
        let good = k.chars().all(|c| c == '_' || c.is_alphanumeric())
            && !k.chars().next().is_some_and(|c| c.is_numeric());
        if !good {
            return Err(Error(format!(
                "parameter names are letters, digits and _ (not starting with a digit): '{k}'"
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn form() -> Value {
        json!({"components": [
            {"type": "panel", "input": false, "key": "p", "components": [
                {"type": "number", "input": true, "key": "plank", "label": "Plank width",
                 "defaultValue": 6, "validate": {"min": 1.5, "max": 40}},
                {"type": "columns", "input": false, "key": "c", "columns": [
                    {"components": [{"type": "number", "input": true, "key": "ratio", "label": "Ratio",
                                     "validate": {"min": 2, "max": 8, "integer": true}}]},
                    {"components": [{"type": "select", "input": true, "key": "layout", "label": "Layout",
                                     "data": {"values": [{"label": "Diagonal", "value": "diagonal"},
                                                         {"label": "Square", "value": "square"}]}}]}]}]},
            {"type": "checkbox", "input": true, "key": "fit", "label": "Fit"},
            {"type": "button", "input": true, "key": "submit"}
        ]})
    }

    #[test]
    fn reads_inputs_and_defaults() {
        let f = form();
        let keys: Vec<&str> = inputs(&f).into_iter().map(key).collect();
        assert_eq!(keys, ["plank", "ratio", "layout", "fit"]);
        let d = defaults(&f);
        assert_eq!(d["plank"], json!(6));
        assert_eq!(d["ratio"], json!(2));
        assert_eq!(d["layout"], json!("diagonal"));
        assert_eq!(d["fit"], json!(false));
    }

    #[test]
    fn checks_values() {
        let f = form();
        let v = check(
            &f,
            Some(&json!({"plank": "7.5", "ratio": 3.5, "fit": 1})),
            "pattern",
        )
        .unwrap();
        assert_eq!(v["plank"], json!(7.5));
        assert_eq!(v["ratio"], json!(4)); // integer: rounded half to even
        assert_eq!(v["fit"], json!(true));
        assert_eq!(v["layout"], json!("diagonal"));
        let e = check(&f, Some(&json!({"plank": 50})), "pattern").unwrap_err();
        assert_eq!(e.0, "Plank width must be between 1.5 and 40");
        let e = check(&f, Some(&json!({"layout": "round"})), "pattern").unwrap_err();
        assert_eq!(e.0, "Layout must be one of diagonal, square");
        assert!(check(&f, Some(&json!({"plank": "wide"})), "pattern").is_err());
        assert!(check(&f, Some(&json!([1])), "preset")
            .unwrap_err()
            .0
            .contains("preset values"));
    }

    #[test]
    fn validates_forms() {
        assert!(validate_form(&form()).is_ok());
        assert!(validate_form(&json!([])).is_err());
        let dup = json!({"components": [
            {"type": "number", "input": true, "key": "a"}, {"type": "number", "input": true, "key": "a"}]});
        assert!(validate_form(&dup).unwrap_err().0.contains("used twice: a"));
        let bad = json!({"components": [{"type": "number", "input": true, "key": "2x"}]});
        assert!(validate_form(&bad).is_err());
        let bad = json!({"components": [{"type": "number", "input": true, "key": "a-b"}]});
        assert!(validate_form(&bad).is_err());
    }
}
