//! The slicer's own tool-change macro, expanded as Bambu Studio does (ADR
//! 0002). A port of slicer_macro.py (the header reading is in slicer.rs).
//!
//! `expand` interprets the slicer macro language as Bambu Studio implements
//! it (PlaceholderParser.cpp): `[var]`, `[var_index]`, `[var[idx_var]]`,
//! `{expr}`, `{if}/{elsif}/{else}/{endif}`, vectors and indexing, int/double
//! arithmetic with C++ semantics, comparisons, `&& || !`/`and or not`, `?:`,
//! string comparison, `min max int round floor ceil`. Numbers print like C++
//! `ostream << double` (6 significant digits).
//!
//! `toolchange_variables` gives the per-change placeholders Bambu Studio sets
//! before expanding `change_filament_gcode`; `find_tool_changes` and
//! `drop_redundant_feedrates` read Bambu's own expansions in a sliced file and
//! reproduce the rewrite Bambu applies to them afterwards (its cooling pass
//! drops `F` words that repeat the current feedrate).
//!
//! Bambu Studio keeps some placeholder values in C++ `float`; `f32r` mimics
//! that so the 6-digit output rounds the same way.

use crate::slicer::{SlicerConfig, Value};
use std::collections::HashMap;

/// The macro could not be expanded (syntax, type or unknown variable).
#[derive(Debug, Clone, PartialEq)]
pub struct MacroError {
    pub message: String,
    /// A placeholder Bambu has but this module cannot derive.
    pub missing: Option<String>,
}

impl std::fmt::Display for MacroError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for MacroError {}

pub type Result<T> = std::result::Result<T, MacroError>;

fn err<T>(m: impl Into<String>) -> Result<T> {
    Err(MacroError {
        message: m.into(),
        missing: None,
    })
}

fn missing_err<T>(name: &str, why: &str) -> Result<T> {
    let mut message = format!("placeholder '{name}' is not derivable");
    if !why.is_empty() {
        message += &format!(": {why}");
    }
    Err(MacroError {
        message,
        missing: Some(name.to_string()),
    })
}

/// A config value with its Bambu option type (`Float`, `Ints`, ...).
#[derive(Debug, Clone, PartialEq)]
pub struct Opt {
    pub kind: String,
    pub value: Value,
}

impl Opt {
    pub fn new(kind: &str, value: Value) -> Self {
        Opt {
            kind: kind.to_string(),
            value,
        }
    }
}

/// A per-call variable: a value, or one Bambu has that can't be derived here.
#[derive(Debug, Clone, PartialEq)]
pub enum Var {
    Set(Opt),
    Missing(String),
}

pub type Vars = HashMap<String, Var>;

fn is_vector(kind: &str) -> bool {
    matches!(
        kind,
        "Floats"
            | "Ints"
            | "Bools"
            | "Strings"
            | "Percents"
            | "FloatsOrPercents"
            | "Points"
            | "Enums"
            | "PointsGroups"
    )
}

/// Round to C++ float.
pub fn f32r(x: f64) -> f64 {
    x as f32 as f64
}

/// C++ `std::ostringstream << double` with default precision (`%g`).
pub fn fmt_double(x: f64) -> String {
    crate::svg::fmt_g(x)
}

// --------------------------------------------------------------------------- //
// values
// --------------------------------------------------------------------------- //

#[derive(Debug, Clone, PartialEq)]
enum Val {
    Bool(bool),
    Int(i64),
    Double(f64),
    Str(String),
}

impl Val {
    fn numeric(&self) -> bool {
        matches!(self, Val::Int(_) | Val::Double(_))
    }

    fn as_d(&self) -> f64 {
        match self {
            Val::Int(i) => *i as f64,
            Val::Double(d) => *d,
            Val::Bool(b) => *b as i64 as f64,
            Val::Str(_) => 0.0,
        }
    }

    fn to_text(&self) -> String {
        match self {
            Val::Bool(b) => if *b { "true" } else { "false" }.into(),
            Val::Int(i) => i.to_string(),
            Val::Double(d) => fmt_double(*d),
            Val::Str(s) => s.clone(),
        }
    }
}

fn serialize_scalar(o: &Opt) -> Result<String> {
    Ok(match (&o.value, o.kind.as_str()) {
        (Value::Float(v), _) => fmt_double(*v),
        (Value::Percent(v), _) => fmt_double(*v) + "%",
        (Value::Int(v), _) => v.to_string(),
        (Value::Bool(b), _) => if *b { "1" } else { "0" }.into(),
        (Value::FloatOrPercent(v, p), _) => fmt_double(*v) + if *p { "%" } else { "" },
        (Value::Point(x, y), _) => format!("{}x{}", fmt_double(*x), fmt_double(*y)),
        (Value::Str(s), _) => s
            .replace('\\', "\\\\")
            .replace('\n', "\\n")
            .replace('\r', "\\r"),
        _ => return err(format!("cannot serialize {}", o.kind)),
    })
}

fn vec_len(v: &Value) -> usize {
    match v {
        Value::Floats(x) | Value::Percents(x) => x.len(),
        Value::Ints(x) => x.len(),
        Value::Bools(x) => x.len(),
        Value::FloatsOrPercents(x) => x.len(),
        Value::Points(x) => x.len(),
        Value::Strs(x) => x.len(),
        _ => 0,
    }
}

fn serialize_elem(o: &Opt, idx: i64) -> Result<String> {
    let n = vec_len(&o.value);
    if n == 0 {
        return err("Indexing an empty vector variable");
    }
    let i = if idx >= 0 && (idx as usize) < n {
        idx as usize
    } else {
        0
    };
    let nil = || "nil".to_string();
    Ok(match &o.value {
        Value::Floats(v) => v[i].map_or_else(nil, fmt_double),
        Value::Percents(v) => v[i].map_or_else(nil, |x| fmt_double(x) + "%"),
        Value::FloatsOrPercents(v) => {
            v[i].map_or_else(nil, |(x, p)| fmt_double(x) + if p { "%" } else { "" })
        }
        Value::Ints(v) => v[i].map_or_else(nil, |x| x.to_string()),
        Value::Bools(v) => v[i].map_or_else(nil, |b| if b { "1" } else { "0" }.into()),
        Value::Points(v) => format!("{}x{}", fmt_double(v[i].0), fmt_double(v[i].1)),
        Value::Strs(v) => v[i].clone(),
        _ => return err(format!("cannot serialize {}", o.kind)),
    })
}

// --------------------------------------------------------------------------- //
// expressions
// --------------------------------------------------------------------------- //

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Num(String),
    Str(String),
    Id(String),
    Op(String),
}

fn tokenize(src: &str) -> Result<Vec<Tok>> {
    let b = src.as_bytes();
    let end = src.trim_end().len();
    let mut pos = 0;
    let mut out = Vec::new();
    let digits = |mut i: usize| {
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
        }
        i
    };
    let exponent = |i: usize| -> Option<usize> {
        if i < b.len() && (b[i] == b'e' || b[i] == b'E') {
            let mut j = i + 1;
            if j < b.len() && (b[j] == b'+' || b[j] == b'-') {
                j += 1;
            }
            let k = digits(j);
            if k > j {
                return Some(k);
            }
        }
        None
    };
    while pos < end {
        while pos < b.len() && (b[pos] as char).is_ascii_whitespace() {
            pos += 1;
        }
        if pos >= end {
            break;
        }
        let c = b[pos];
        let start = pos;
        if c.is_ascii_digit() || (c == b'.' && pos + 1 < b.len() && b[pos + 1].is_ascii_digit()) {
            let d = digits(pos);
            let stop = if d < b.len() && b[d] == b'.' && d > pos {
                let f = digits(d + 1);
                exponent(f).unwrap_or(f)
            } else if c == b'.' {
                let f = digits(pos + 1);
                exponent(f).unwrap_or(f)
            } else {
                exponent(d).unwrap_or(d)
            };
            out.push(Tok::Num(src[start..stop].to_string()));
            pos = stop;
        } else if c == b'"' {
            let mut j = pos + 1;
            while j < b.len() && b[j] != b'"' {
                j += if b[j] == b'\\' { 2 } else { 1 };
            }
            if j >= b.len() {
                return err(format!("cannot parse '{}' in {{{src}}}", &src[pos..]));
            }
            out.push(Tok::Str(src[pos..=j].to_string()));
            pos = j + 1;
        } else if c.is_ascii_alphabetic() || c == b'_' {
            let mut j = pos;
            while j < b.len() && (b[j].is_ascii_alphanumeric() || b[j] == b'_') {
                j += 1;
            }
            out.push(Tok::Id(src[pos..j].to_string()));
            pos = j;
        } else {
            let two = src.get(pos..pos + 2).unwrap_or("");
            if ["||", "&&", "==", "!=", "<>", "<=", ">=", "=~", "!~"].contains(&two) {
                out.push(Tok::Op(two.to_string()));
                pos += 2;
            } else if "-+*/%()<>!?:,[]".contains(c as char) {
                out.push(Tok::Op((c as char).to_string()));
                pos += 1;
            } else {
                return err(format!("cannot parse '{}' in {{{src}}}", &src[pos..]));
            }
        }
    }
    Ok(out)
}

struct Ctx<'a> {
    vars: &'a Vars,
    config: Option<&'a SlicerConfig>,
    current_extruder_id: i64,
}

struct Expr<'a, 'b> {
    src: &'a str,
    toks: Vec<Tok>,
    i: usize,
    ctx: &'b Ctx<'b>,
}

impl<'a, 'b> Expr<'a, 'b> {
    fn new(src: &'a str, ctx: &'b Ctx<'b>) -> Result<Self> {
        Ok(Expr {
            src,
            toks: tokenize(src)?,
            i: 0,
            ctx,
        })
    }

    fn peek(&self, k: usize) -> Option<&Tok> {
        self.toks.get(self.i + k)
    }

    fn is_op(&self, k: usize, op: &str) -> bool {
        matches!(self.peek(k), Some(Tok::Op(o)) if o == op)
    }

    fn accept(&mut self, ops: &[&str]) -> Option<String> {
        let hit = match self.peek(0) {
            Some(Tok::Op(o)) | Some(Tok::Id(o)) if ops.contains(&o.as_str()) => Some(o.clone()),
            _ => None,
        };
        if hit.is_some() {
            self.i += 1;
        }
        hit
    }

    fn expect(&mut self, op: &str) -> Result<()> {
        if self.accept(&[op]).is_none() {
            return err(format!("expected '{op}' in {{{}}}", self.src));
        }
        Ok(())
    }

    fn done(&self) -> Result<()> {
        match self.peek(0) {
            None => Ok(()),
            Some(t) => err(format!("unexpected '{}' in {{{}}}", tok_text(t), self.src)),
        }
    }

    fn conditional(&mut self) -> Result<Val> {
        let c = self.logical_or()?;
        if self.accept(&["?"]).is_some() {
            let a = self.conditional()?;
            self.expect(":")?;
            let b = self.conditional()?;
            return match c {
                Val::Bool(true) => Ok(a),
                Val::Bool(false) => Ok(b),
                _ => err("Not a boolean expression"),
            };
        }
        Ok(c)
    }

    fn logical_or(&mut self) -> Result<Val> {
        let mut a = self.logical_and()?;
        while self.accept(&["||", "or"]).is_some() {
            let b = self.logical_and()?;
            a = logic(a, b, true)?;
        }
        Ok(a)
    }

    fn logical_and(&mut self) -> Result<Val> {
        let mut a = self.equality()?;
        while self.accept(&["&&", "and"]).is_some() {
            let b = self.equality()?;
            a = logic(a, b, false)?;
        }
        Ok(a)
    }

    fn equality(&mut self) -> Result<Val> {
        let mut a = self.relational()?;
        while let Some(op) = self.accept(&["==", "!=", "<>", "=~", "!~"]) {
            if op == "=~" || op == "!~" {
                return err("regular expressions are not supported");
            }
            let b = self.relational()?;
            a = compare(&a, &b, '=', op != "==")?;
        }
        Ok(a)
    }

    fn relational(&mut self) -> Result<Val> {
        let mut a = self.additive()?;
        while let Some(op) = self.accept(&["<=", ">=", "<", ">"]) {
            let b = self.additive()?;
            a = match op.as_str() {
                "<=" => compare(&a, &b, '>', true)?,
                ">=" => compare(&a, &b, '<', true)?,
                "<" => compare(&a, &b, '<', false)?,
                _ => compare(&a, &b, '>', false)?,
            };
        }
        Ok(a)
    }

    fn additive(&mut self) -> Result<Val> {
        let mut a = self.multiplicative()?;
        while let Some(op) = self.accept(&["+", "-"]) {
            let b = self.multiplicative()?;
            if op == "+" && (matches!(a, Val::Str(_)) || matches!(b, Val::Str(_))) {
                a = Val::Str(a.to_text() + &b.to_text());
                continue;
            }
            a = arith(&a, &b, &op)?;
        }
        Ok(a)
    }

    fn multiplicative(&mut self) -> Result<Val> {
        let mut a = self.unary()?;
        while let Some(op) = self.accept(&["*", "/", "%"]) {
            let b = self.unary()?;
            a = arith(&a, &b, &op)?;
        }
        Ok(a)
    }

    fn unary(&mut self) -> Result<Val> {
        let t = self.peek(0).cloned();
        match t {
            Some(Tok::Op(o)) if o == "(" => {
                self.i += 1;
                let r = self.conditional()?;
                self.expect(")")?;
                Ok(r)
            }
            Some(Tok::Op(o)) if o == "-" => {
                self.i += 1;
                match self.unary()? {
                    Val::Int(i) => Ok(Val::Int(-i)),
                    Val::Double(d) => Ok(Val::Double(-d)),
                    _ => err("Cannot apply unary minus operator."),
                }
            }
            Some(Tok::Op(o)) if o == "+" => {
                self.i += 1;
                self.unary()
            }
            Some(Tok::Op(o)) if o == "!" => self.not(),
            Some(Tok::Id(o)) if o == "not" => self.not(),
            Some(Tok::Num(v)) => {
                self.i += 1;
                if v.bytes().all(|c| c.is_ascii_digit()) {
                    if let Ok(i) = v.parse::<i64>() {
                        return Ok(Val::Int(i));
                    }
                }
                Ok(Val::Double(v.parse().unwrap_or(0.0)))
            }
            Some(Tok::Str(v)) => {
                self.i += 1;
                let body = &v[1..v.len() - 1];
                let mut s = String::new();
                let mut it = body.chars();
                while let Some(c) = it.next() {
                    if c == '\\' {
                        if let Some(n) = it.next() {
                            s.push(n);
                            continue;
                        }
                    }
                    s.push(c);
                }
                Ok(Val::Str(s))
            }
            Some(Tok::Id(v)) => {
                if v == "true" || v == "false" {
                    self.i += 1;
                    return Ok(Val::Bool(v == "true"));
                }
                if (v == "min" || v == "max") && self.is_op(1, "(") {
                    self.i += 2;
                    let a = self.conditional()?;
                    self.expect(",")?;
                    let b = self.conditional()?;
                    self.expect(")")?;
                    if !(a.numeric() && b.numeric()) {
                        return err("Not a numeric type.");
                    }
                    let pick_min = v == "min";
                    if let (Val::Int(x), Val::Int(y)) = (&a, &b) {
                        return Ok(Val::Int(if pick_min { *x.min(y) } else { *x.max(y) }));
                    }
                    let (x, y) = (a.as_d(), b.as_d());
                    // Python's min/max keep the first of equal values
                    let r = if pick_min {
                        if y < x {
                            y
                        } else {
                            x
                        }
                    } else if y > x {
                        y
                    } else {
                        x
                    };
                    return Ok(Val::Double(r));
                }
                if ["int", "round", "floor", "ceil"].contains(&v.as_str()) && self.is_op(1, "(") {
                    self.i += 2;
                    let a = self.conditional()?;
                    self.expect(")")?;
                    let x = match a {
                        Val::Int(_) => return Ok(a),
                        Val::Double(x) => x,
                        _ => return err(format!("{v}() of a non-numeric value")),
                    };
                    let r = match v.as_str() {
                        "int" => x.trunc(),
                        "round" => (x.abs() + 0.5).floor() * if x >= 0.0 { 1.0 } else { -1.0 },
                        "floor" => x.floor(),
                        _ => x.ceil(),
                    };
                    return Ok(Val::Int(r as i64));
                }
                if [
                    "digits",
                    "zdigits",
                    "random",
                    "filament_change",
                    "is_nil",
                    "one_of",
                    "interpolate_table",
                    "regex_replace",
                    "size",
                    "empty",
                ]
                .contains(&v.as_str())
                {
                    return err(format!("function {v}() is not supported"));
                }
                self.i += 1;
                self.variable(&v)
            }
            Some(t) => err(format!("unexpected '{}' in {{{}}}", tok_text(&t), self.src)),
            None => err(format!("unexpected 'None' in {{{}}}", self.src)),
        }
    }

    fn not(&mut self) -> Result<Val> {
        self.i += 1;
        match self.unary()? {
            Val::Bool(b) => Ok(Val::Bool(!b)),
            _ => err("Cannot apply a not operator."),
        }
    }

    fn variable(&mut self, name: &str) -> Result<Val> {
        let Some(o) = self.ctx.resolve(name)? else {
            return missing_err(name, "not a variable");
        };
        if self.is_op(0, "[") {
            self.i += 1;
            let idx = self.additive()?;
            self.expect("]")?;
            let Val::Int(idx) = idx else {
                return err("Non-integer index is not allowed to address a vector variable.");
            };
            if !is_vector(&o.kind) {
                return err(format!(
                    "Referencing a scalar variable '{name}' when vector is expected"
                ));
            }
            let n = vec_len(&o.value);
            if n == 0 {
                return err("Indexing an empty vector variable");
            }
            let i = if idx >= 0 && (idx as usize) < n {
                idx as usize
            } else {
                0
            };
            return Ok(match (&o.value, o.kind.as_str()) {
                (Value::Floats(v), _) | (Value::Percents(v), _) => Val::Double(v[i].unwrap_or(0.0)),
                (Value::Ints(v), _) => Val::Int(v[i].unwrap_or(0)),
                (Value::Bools(v), _) => Val::Bool(v[i].unwrap_or(false)),
                (Value::Strs(v), "Strings") => Val::Str(v[i].clone()),
                (Value::Points(v), _) => {
                    Val::Str(format!("{}x{}", fmt_double(v[i].0), fmt_double(v[i].1)))
                }
                _ => return err(format!("Unknown vector variable type {}", o.kind)),
            });
        }
        match &o.value {
            Value::Floats(v) => {
                let e = if v.len() == 1 {
                    v[0]
                } else {
                    v.get(self.ctx.extruder_id()?)
                        .or(v.first())
                        .copied()
                        .flatten()
                };
                e.map(Val::Double).ok_or_else(|| MacroError {
                    message: format!("nil value of '{name}'"),
                    missing: None,
                })
            }
            Value::FloatsOrPercents(v) => {
                let e = if v.len() == 1 {
                    v[0]
                } else {
                    v.get(self.ctx.extruder_id()?)
                        .or(v.first())
                        .copied()
                        .flatten()
                };
                match e {
                    Some((_, true)) => err(format!("percent value of '{name}' is not supported")),
                    Some((x, false)) => Ok(Val::Double(x)),
                    None => err(format!("nil value of '{name}'")),
                }
            }
            _ if is_vector(&o.kind) => {
                err(format!("Unknown scalar variable type {} ({name})", o.kind))
            }
            Value::Float(x) => {
                if name.ends_with("line_width") {
                    return err("line width variables are not supported");
                }
                Ok(Val::Double(*x))
            }
            Value::Int(i) => Ok(Val::Int(*i)),
            Value::Str(s) => Ok(Val::Str(s.clone())),
            Value::Percent(x) => Ok(Val::Double(*x)),
            Value::Point(..) => Ok(Val::Str(serialize_scalar(&o)?)),
            Value::Bool(b) => Ok(Val::Bool(*b)),
            Value::FloatOrPercent(x, p) => {
                if *p {
                    return err(format!("percent value of '{name}' is not supported"));
                }
                Ok(Val::Double(*x))
            }
            _ => err(format!("Unknown scalar variable type {}", o.kind)),
        }
    }
}

fn tok_text(t: &Tok) -> &str {
    match t {
        Tok::Num(s) | Tok::Str(s) | Tok::Id(s) | Tok::Op(s) => s,
    }
}

fn logic(a: Val, b: Val, or: bool) -> Result<Val> {
    match (a, b) {
        (Val::Bool(x), Val::Bool(y)) => Ok(Val::Bool(if or { x || y } else { x && y })),
        _ => err("Cannot apply logical operation to non-boolean operators."),
    }
}

fn compare(a: &Val, b: &Val, op: char, invert: bool) -> Result<Val> {
    let r = if a.numeric() && b.numeric() {
        match (a, b) {
            (Val::Int(x), Val::Int(y)) => match op {
                '=' => x == y,
                '<' => x < y,
                _ => x > y,
            },
            _ => {
                let (x, y) = (a.as_d(), b.as_d());
                match op {
                    '=' => (x - y).abs() < 1e-8,
                    '<' => x < y,
                    _ => x > y,
                }
            }
        }
    } else if let (Val::Bool(x), Val::Bool(y)) = (a, b) {
        if op != '=' {
            return err("Cannot compare the types.");
        }
        x == y
    } else if matches!(a, Val::Str(_)) || matches!(b, Val::Str(_)) {
        let (x, y) = (a.to_text(), b.to_text());
        match op {
            '=' => x == y,
            '<' => x < y,
            _ => x > y,
        }
    } else {
        return err("Cannot compare the types.");
    };
    Ok(Val::Bool(if invert { !r } else { r }))
}

fn arith(a: &Val, b: &Val, op: &str) -> Result<Val> {
    if !(a.numeric() && b.numeric()) {
        return err(format!("non-numeric operand for '{op}'"));
    }
    if (op == "/" || op == "%") && b.as_d() == 0.0 {
        return err("Division by zero");
    }
    if let (Val::Int(x), Val::Int(y)) = (a, b) {
        let r = match op {
            "+" => x.checked_add(*y),
            "-" => x.checked_sub(*y),
            "*" => x.checked_mul(*y),
            "/" => x.checked_div(*y),
            _ => x.checked_rem(*y),
        };
        return r.map(Val::Int).ok_or_else(|| MacroError {
            message: "integer overflow".into(),
            missing: None,
        });
    }
    let (x, y) = (a.as_d(), b.as_d());
    Ok(Val::Double(match op {
        "+" => x + y,
        "-" => x - y,
        "*" => x * y,
        "/" => x / y,
        _ => x % y,
    }))
}

impl Ctx<'_> {
    fn resolve(&self, name: &str) -> Result<Option<Opt>> {
        match self.vars.get(name) {
            Some(Var::Missing(why)) => missing_err(name, why),
            Some(Var::Set(o)) => Ok(Some(o.clone())),
            None => Ok(self.config.and_then(|c| {
                c.get(name).map(|v| Opt {
                    kind: c.kinds.get(name).cloned().unwrap_or_default(),
                    value: v.clone(),
                })
            })),
        }
    }

    /// PlaceholderParser's get_extruder_id(): filament_map[current] (1-based!) or 0.
    fn extruder_id(&self) -> Result<usize> {
        if let Some(Opt {
            kind,
            value: Value::Ints(v),
        }) = self.resolve("filament_map")?
        {
            if kind == "Ints"
                && self.current_extruder_id >= 0
                && (self.current_extruder_id as usize) < v.len()
            {
                return Ok(v[self.current_extruder_id as usize].unwrap_or(0).max(0) as usize);
            }
        }
        Ok(0)
    }

    fn legacy(&self, name: &str, index_var: Option<&str>) -> Result<String> {
        if let Some(iv) = index_var {
            let mut o = self.resolve(name)?;
            if o.is_none() && name.ends_with('_') {
                o = self.resolve(&name[..name.len() - 1])?;
            }
            let Some(o) = o.filter(|o| is_vector(&o.kind)) else {
                return err(format!("Trying to index a scalar variable '{name}'"));
            };
            let Some(io) = self.resolve(iv)? else {
                return missing_err(iv, "not a variable");
            };
            let Value::Int(i) = io.value else {
                return err("Indexing variable has to be integer");
            };
            if i < 0 {
                return err("Negative vector index");
            }
            return serialize_elem(&o, i);
        }
        let mut o = self.resolve(name)?;
        let mut idx = self.current_extruder_id;
        if o.is_none() {
            if let Some((base, num)) = name.rsplit_once('_') {
                if !base.is_empty() && !num.is_empty() && num.bytes().all(|c| c.is_ascii_digit()) {
                    if let Some(b) = self.resolve(base)? {
                        if !is_vector(&b.kind) {
                            return err("Trying to index a scalar variable");
                        }
                        idx = num.parse().unwrap_or(0);
                        o = Some(b);
                    }
                }
            }
        }
        let Some(o) = o else {
            return missing_err(name, "not a variable");
        };
        if !is_vector(&o.kind) {
            return serialize_scalar(&o);
        }
        serialize_elem(&o, idx)
    }
}

// --------------------------------------------------------------------------- //
// templates
// --------------------------------------------------------------------------- //

#[derive(Debug)]
enum Node {
    Text(String),
    Expr(String),
    Legacy(String, Option<String>),
    If {
        branches: Vec<(String, Vec<Node>)>,
        else_body: Option<Vec<Node>>,
    },
}

fn body_of(root: &mut Vec<Node>, depth: usize) -> &mut Vec<Node> {
    let mut cur = root;
    for _ in 0..depth {
        let Some(Node::If {
            branches,
            else_body,
        }) = cur.last_mut()
        else {
            unreachable!("open {{if}} is the last node");
        };
        cur = match else_body {
            Some(e) => e,
            None => &mut branches.last_mut().unwrap().1,
        };
    }
    cur
}

/// `[name]` or `[name[index_var]]` at `i`: (name, index var, end).
fn legacy_at(src: &str, i: usize) -> Option<(String, Option<String>, usize)> {
    let b = src.as_bytes();
    let ws = |mut j: usize| {
        while j < b.len() && (b[j] as char).is_whitespace() {
            j += 1;
        }
        j
    };
    let ident = |j: usize| -> Option<usize> {
        if j < b.len() && (b[j].is_ascii_alphabetic() || b[j] == b'_') {
            let mut k = j + 1;
            while k < b.len() && (b[k].is_ascii_alphanumeric() || b[k] == b'_') {
                k += 1;
            }
            Some(k)
        } else {
            None
        }
    };
    let j = ws(i + 1);
    let k = ident(j)?;
    let name = src[j..k].to_string();
    let mut p = ws(k);
    let mut index = None;
    if p < b.len() && b[p] == b'[' {
        let q = ws(p + 1);
        if let Some(r) = ident(q) {
            let s = ws(r);
            if s < b.len() && b[s] == b']' {
                index = Some(src[q..r].to_string());
                p = ws(s + 1);
            }
        }
    }
    (p < b.len() && b[p] == b']').then_some((name, index, p + 1))
}

/// `if ...` / `elsif ...`: (keyword, condition).
fn if_head(inner: &str) -> Option<(&str, &str)> {
    for kw in ["elsif", "if"] {
        if let Some(rest) = inner.strip_prefix(kw) {
            if !rest.starts_with(|c: char| c.is_ascii_alphanumeric() || c == '_') {
                return Some((kw, rest));
            }
        }
    }
    None
}

fn parse_template(src: &str) -> Result<Vec<Node>> {
    let mut root: Vec<Node> = Vec::new();
    // open {if}s: how many have an {else} started is in the nodes themselves
    let mut depth = 0usize;
    let mut buf = String::new();
    let b = src.as_bytes();
    let n = src.len();
    let mut i = 0;
    let flush = |root: &mut Vec<Node>, depth: usize, buf: &mut String| {
        if !buf.is_empty() {
            body_of(root, depth).push(Node::Text(std::mem::take(buf)));
        }
    };
    while i < n {
        let c = b[i];
        if c == b'{' {
            flush(&mut root, depth, &mut buf);
            let (mut j, mut inq) = (i + 1, false);
            while j < n && (inq || b[j] != b'}') {
                if b[j] == b'"' && b[j - 1] != b'\\' {
                    inq = !inq;
                }
                j += 1;
            }
            if j >= n {
                return err("unterminated {");
            }
            let inner = src[i + 1..j].trim();
            i = j + 1;
            match if_head(inner) {
                Some(("if", cond)) => {
                    body_of(&mut root, depth).push(Node::If {
                        branches: vec![(cond.trim().to_string(), Vec::new())],
                        else_body: None,
                    });
                    depth += 1;
                }
                Some((_, cond)) => {
                    let open = depth > 0
                        && matches!(
                            body_of(&mut root, depth - 1).last(),
                            Some(Node::If {
                                else_body: None,
                                ..
                            })
                        );
                    if !open {
                        return err("{elsif} without {if}");
                    }
                    if let Some(Node::If { branches, .. }) =
                        body_of(&mut root, depth - 1).last_mut()
                    {
                        branches.push((cond.trim().to_string(), Vec::new()));
                    }
                }
                None if inner == "else" => {
                    let open = depth > 0
                        && matches!(
                            body_of(&mut root, depth - 1).last(),
                            Some(Node::If {
                                else_body: None,
                                ..
                            })
                        );
                    if !open {
                        return err("{else} without {if}");
                    }
                    if let Some(Node::If { else_body, .. }) =
                        body_of(&mut root, depth - 1).last_mut()
                    {
                        *else_body = Some(Vec::new());
                    }
                }
                None if inner == "endif" => {
                    if depth == 0 {
                        return err("{endif} without {if}");
                    }
                    depth -= 1;
                }
                None => body_of(&mut root, depth).push(Node::Expr(inner.to_string())),
            }
        } else if c == b'[' {
            flush(&mut root, depth, &mut buf);
            let Some((name, index, end)) = legacy_at(src, i) else {
                let snippet: String = src[i..].chars().take(30).collect();
                return err(format!("bad legacy variable at '{snippet}'"));
            };
            body_of(&mut root, depth).push(Node::Legacy(name, index));
            i = end;
        } else {
            let ch = src[i..].chars().next().unwrap();
            buf.push(ch);
            i += ch.len_utf8();
        }
    }
    flush(&mut root, depth, &mut buf);
    if depth > 0 {
        return err("{if} without {endif}");
    }
    Ok(root)
}

fn render(nodes: &[Node], ctx: &Ctx, out: &mut String) -> Result<()> {
    for nd in nodes {
        match nd {
            Node::Text(t) => out.push_str(t),
            Node::If {
                branches,
                else_body,
            } => {
                let mut taken = false;
                for (cond, sub) in branches {
                    let mut ex = Expr::new(cond, ctx)?;
                    let v = ex.conditional()?;
                    ex.done()?;
                    let Val::Bool(v) = v else {
                        return err(format!("Not a boolean expression: {cond}"));
                    };
                    if v {
                        render(sub, ctx, out)?;
                        taken = true;
                        break;
                    }
                }
                if !taken {
                    if let Some(e) = else_body {
                        render(e, ctx, out)?;
                    }
                }
            }
            Node::Expr(s) => {
                let mut ex = Expr::new(s, ctx)?;
                let v = ex.additive()?;
                ex.done()?;
                out.push_str(&v.to_text());
            }
            Node::Legacy(name, index) => out.push_str(&ctx.legacy(name, index.as_deref())?),
        }
    }
    Ok(())
}

/// Expand a slicer macro. Variables are looked up in `vars` first, then in
/// `config`, as Bambu does. `current_extruder_id` is the filament Bambu
/// passes to the parser (for change_filament_gcode the *next* filament); it
/// picks the element of an un-indexed vector.
pub fn expand(
    template: &str,
    vars: &Vars,
    config: Option<&SlicerConfig>,
    current_extruder_id: i64,
) -> Result<String> {
    let ctx = Ctx {
        vars,
        config,
        current_extruder_id,
    };
    let mut out = String::new();
    render(&parse_template(template)?, &ctx, &mut out)?;
    Ok(out)
}

// --------------------------------------------------------------------------- //
// tool-change placeholders
// --------------------------------------------------------------------------- //

/// GCode.cpp g_min_purge_volume (prime tower only).
const G_MIN_PURGE_VOLUME: f64 = 100.0;
const G_PURGE_VOLUME_ONE_TIME: f64 = 135.0;
const G_MAX_FLUSH_COUNT: usize = 4;

fn area(d: f64) -> f64 {
    f32r(std::f64::consts::FRAC_PI_4 * d * d)
}

fn at(cfg: &SlicerConfig, key: &str, idx: usize) -> Option<f64> {
    cfg.at_f64(key, idx)
}

/// Python truthiness of a setting (`cfg.get(key)`): a vector is true when not empty.
fn truthy(cfg: &SlicerConfig, key: &str) -> bool {
    match cfg.get(key) {
        None => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Int(i)) => *i != 0,
        Some(Value::Float(x)) | Some(Value::Percent(x)) => *x != 0.0,
        Some(Value::Str(s)) | Some(Value::Raw(s)) => !s.is_empty(),
        Some(v) => vec_len(v) > 0,
    }
}

fn n_filaments(cfg: &SlicerConfig) -> usize {
    cfg.get("filament_colour").map(vec_len).unwrap_or(0)
}

fn element(v: &Value, i: usize) -> Option<f64> {
    match v {
        Value::Floats(x) | Value::Percents(x) => x.get(i).copied().flatten(),
        Value::Ints(x) => x.get(i).copied().flatten().map(|v| v as f64),
        Value::Bools(x) => x.get(i).copied().flatten().map(|b| b as i64 as f64),
        Value::FloatsOrPercents(x) => x.get(i).copied().flatten().map(|v| v.0),
        _ => None,
    }
}

/// Per-filament value after Bambu merges filament overrides into the machine
/// value: `filament_<key>` wins where it is not nil (for the long-retraction
/// keys only when enable_long_retraction_when_cut is 2 = per filament).
fn effective(cfg: &SlicerConfig, key: &str, fil: usize) -> f64 {
    let use_filament =
        if key == "long_retractions_when_cut" || key == "retraction_distances_when_cut" {
            cfg.f64("enable_long_retraction_when_cut").unwrap_or(0.0) == 2.0
        } else {
            true
        };
    if use_filament {
        if let Some(v) = cfg
            .get(&format!("filament_{key}"))
            .and_then(|fv| element(fv, fil))
        {
            return v;
        }
    }
    at(cfg, key, 0).unwrap_or(0.0)
}

/// Volume (mm3) Bambu purges in the macro for a prev -> next change (float, like Bambu).
pub fn flush_volume(cfg: &SlicerConfig, prev: usize, nxt: usize, wipe_tower: bool) -> f64 {
    let n = n_filaments(cfg);
    let fast = cfg.get("prime_volume_mode").and_then(Value::as_str) == Some("Fast");
    let mult = at(
        cfg,
        if fast {
            "flush_multiplier_fast"
        } else {
            "flush_multiplier"
        },
        0,
    )
    .unwrap_or(1.0);
    // the header's matrix is already multiplied by flush_multiplier; recover
    // the stored value so the float arithmetic matches Bambu's
    let shown = cfg
        .get("flush_volumes_matrix")
        .and_then(|m| element(m, prev * n + nxt))
        .unwrap_or(0.0);
    let raw = if mult != 0.0 {
        py_round4(shown / mult)
    } else {
        shown
    };
    let mut v = f32r(f32r(raw) * if wipe_tower { f32r(mult) } else { mult });
    let grab = at(cfg, "grab_length", 0).unwrap_or(0.0);
    if grab != 0.0 {
        let d = at(cfg, "filament_diameter", nxt).unwrap_or(1.75);
        v = (f32r(v - f32r(grab * (std::f64::consts::FRAC_PI_4 * d * d)))).max(0.0);
    }
    if wipe_tower {
        v = if v < 1e-6 {
            0.0
        } else {
            v.max(G_MIN_PURGE_VOLUME)
        };
    }
    v
}

/// Python's round(x, 4): to the nearest, ties to even, of the decimal value.
fn py_round4(x: f64) -> f64 {
    format!("{x:.4}").parse().unwrap_or(x)
}

/// (flush_length, [flush_length_1..4]) as Bambu computes them.
pub fn flush_lengths(
    cfg: &SlicerConfig,
    prev: usize,
    nxt: usize,
    wipe_tower: bool,
) -> (f64, [f64; 4]) {
    let vol = flush_volume(cfg, prev, nxt, wipe_tower);
    let length = f32r(vol / area(at(cfg, "filament_diameter", nxt).unwrap_or(1.75)));
    let q = f32r(vol / G_PURGE_VOLUME_ONE_TIME);
    let mut count = G_MAX_FLUSH_COUNT.min((q + 0.5).floor() as usize);
    if count == 0 && vol > 0.0 {
        count = 1;
    }
    let unit = if count > 0 {
        f32r(length / count as f64)
    } else {
        0.0
    };
    let mut parts = [0.0; 4];
    for (i, p) in parts.iter_mut().enumerate() {
        if i < count {
            *p = unit;
        }
    }
    (length, parts)
}

/// GCode.cpp get_path_of_change_filament: the AMS change path around the
/// objects. `object_bboxes` are (min_x, min_y, max_x, max_y) per object.
pub fn travel_points(
    cfg: &SlicerConfig,
    object_bboxes: Option<&[[f64; 4]]>,
) -> Result<[(f64, f64); 3]> {
    let default = [(54.0, 0.0), (54.0, 0.0), (54.0, 245.0)];
    let pts = match cfg.get("start_end_points") {
        Some(Value::Points(p)) => p.clone(),
        _ => vec![],
    };
    let excl = match cfg.get("bed_exclude_area") {
        Some(Value::Points(p)) => p.clone(),
        _ => vec![],
    };
    if pts.len() != 2 || excl.len() != 4 {
        return Ok(default);
    }
    let Some(boxes) = object_bboxes else {
        return missing_err("travel_point_1_x", "needs the objects' bounding boxes");
    };
    let (start_x, (end_x, end_y)) = (pts[0].0, pts[1]);
    let cutter_y = excl[2].1 + 2.0;
    let mut can_left = true;
    let mut intervals: Vec<(f64, f64)> = Vec::new();
    for &[x0, y0, x1, _] in boxes {
        if x0 < start_x && y0 < cutter_y {
            can_left = false;
        }
        let scope = (x0 - 2.0, x1 + 2.0);
        if intervals.is_empty() {
            intervals.push(scope);
            continue;
        }
        let (mut new, mut hit, mut merged) = (Vec::new(), false, (0.0, 0.0));
        for iv in &intervals {
            if iv.1 >= scope.0 && iv.0 <= scope.1 {
                merged = if hit {
                    (iv.0.min(merged.0), iv.1.max(merged.1))
                } else {
                    (iv.0.min(scope.0), iv.1.max(scope.1))
                };
                hit = true;
            } else {
                new.push(*iv);
            }
        }
        if hit {
            new.push(merged);
            intervals = new;
        } else {
            intervals.push(scope);
        }
    }
    intervals.sort_by(|a, b| a.0.total_cmp(&b.0));
    let (mut avail, mut pos) = (Vec::new(), 0.0);
    for &(a, b) in &intervals {
        if a > pos {
            avail.push((pos, a));
        }
        pos = b;
    }
    avail.push((pos, 255.0));
    let mut path = 255.0;
    for &(a, b) in &avail {
        if a > end_x {
            let dist = a - end_x;
            path = if (end_x - path).abs() < dist { path } else { a };
            break;
        }
        if b >= end_x {
            path = end_x;
            break;
        }
        if !can_left && b < start_x {
            continue;
        }
        path = b;
    }
    let out = if path < start_x {
        [(start_x, cutter_y), (path, cutter_y), (path, end_y)]
    } else {
        [(path, 0.0), (path, 0.0), (path, end_y)]
    };
    Ok(out.map(|(x, y)| (f32r(x), f32r(y))))
}

fn width_of(cfg: &SlicerConfig, key: &str, nozzle: f64) -> f64 {
    match cfg.get(key) {
        Some(Value::FloatOrPercent(v, true)) => v * nozzle / 100.0,
        Some(Value::FloatOrPercent(v, false)) | Some(Value::Float(v)) => *v,
        Some(Value::Percent(v)) => v * nozzle / 100.0,
        Some(v) => v.as_f64().unwrap_or(0.0),
        None => 0.0,
    }
}

/// GCode.cpp get_outer_wall_volumetric_speed (single nozzle).
fn outer_wall_volumetric_speed(cfg: &SlicerConfig, nxt: usize) -> f64 {
    let nozzle = at(cfg, "nozzle_diameter", 0).unwrap_or(0.4);
    let mut w = width_of(cfg, "outer_wall_line_width", nozzle);
    if w == 0.0 {
        w = width_of(cfg, "line_width", nozzle);
        if w == 0.0 {
            w = at(cfg, "filament_diameter", nxt).unwrap_or(0.0);
        }
    }
    let h = cfg.f64("layer_height").unwrap_or(0.2);
    let mm3_per_mm = h * (w - h * (1.0 - 0.25 * std::f64::consts::PI));
    let speed = at(cfg, "outer_wall_speed", 0).unwrap_or(0.0) * mm3_per_mm;
    f32r(speed.min(at(cfg, "filament_max_volumetric_speed", nxt).unwrap_or(f64::INFINITY)))
}

/// Where one tool change sits, for its placeholders.
#[derive(Debug, Clone, Default)]
pub struct ChangeAt {
    pub layer_z: f64,
    pub max_layer_z: f64,
    /// Tool changes counted from 1, including this one.
    pub toolchange_count: i64,
    /// Selects nozzle_temperature_initial_layer over nozzle_temperature.
    pub first_layer: bool,
    pub wipe_tower: bool,
    pub layer_num: Option<i64>,
    pub object_bboxes: Option<Vec<[f64; 4]>>,
    /// The nozzle position before the change (only `x/y/z_after_toolchange`).
    pub position: Option<(f64, f64, f64)>,
}

fn set(v: &mut Vars, key: &str, kind: &str, value: Value) {
    v.insert(key.to_string(), Var::Set(Opt::new(kind, value)));
}

fn miss(v: &mut Vars, key: &str, why: &str) {
    v.insert(key.to_string(), Var::Missing(why.to_string()));
}

fn ints(cfg: &SlicerConfig, key: &str, i: usize) -> Option<i64> {
    at(cfg, key, i).map(|x| x as i64)
}

/// The placeholders Bambu Studio sets for one tool change (prev -> next filament).
pub fn toolchange_variables(cfg: &SlicerConfig, prev: i64, nxt: usize, c: &ChangeAt) -> Vars {
    let n = n_filaments(cfg);
    let mut v = Vars::new();
    let p = prev.max(0) as usize;
    set(&mut v, "previous_extruder", "Int", Value::Int(prev));
    set(&mut v, "next_extruder", "Int", Value::Int(nxt as i64));
    set(&mut v, "current_filament_id", "Int", Value::Int(prev));
    set(&mut v, "next_filament_id", "Int", Value::Int(nxt as i64));
    set(
        &mut v,
        "current_extruder_id",
        "Int",
        Value::Int(if prev >= 0 { 0 } else { -1 }),
    );
    set(&mut v, "next_extruder_id", "Int", Value::Int(0));
    match c.layer_num {
        Some(l) => set(&mut v, "layer_num", "Int", Value::Int(l)),
        None => miss(&mut v, "layer_num", "layer index not given"),
    }
    set(
        &mut v,
        "layer_z",
        "Float",
        Value::Float(if c.wipe_tower {
            f32r(c.layer_z)
        } else {
            c.layer_z
        }),
    );
    set(
        &mut v,
        "max_layer_z",
        "Float",
        Value::Float(f32r(c.max_layer_z)),
    );
    set(
        &mut v,
        "toolchange_count",
        "Int",
        Value::Int(c.toolchange_count),
    );
    set(
        &mut v,
        "relative_e_axis",
        "Bool",
        Value::Bool(truthy(cfg, "use_relative_e_distances")),
    );
    set(&mut v, "fan_speed", "Int", Value::Int(0));

    let first_key = |first: bool| {
        if first {
            "nozzle_temperature_initial_layer"
        } else {
            "nozzle_temperature"
        }
    };
    let new_first = c.first_layer || (!c.wipe_tower && c.layer_z.abs() < 1e-6);
    let old_temp = if prev >= 0 {
        ints(cfg, first_key(c.first_layer), p).unwrap_or(0)
    } else if c.wipe_tower {
        210
    } else {
        0
    };
    set(&mut v, "old_filament_temp", "Int", Value::Int(old_temp));
    set(
        &mut v,
        "new_filament_temp",
        "Int",
        Value::Int(ints(cfg, first_key(new_first), nxt).unwrap_or(0)),
    );

    set(
        &mut v,
        "old_retract_length",
        "Float",
        Value::Float(f32r(effective(cfg, "retraction_length", p))),
    );
    set(
        &mut v,
        "new_retract_length",
        "Float",
        Value::Float(f32r(effective(cfg, "retraction_length", nxt))),
    );
    set(
        &mut v,
        "old_retract_length_toolchange",
        "Float",
        Value::Float(f32r(effective(cfg, "retract_length_toolchange", p))),
    );
    set(
        &mut v,
        "new_retract_length_toolchange",
        "Float",
        Value::Float(f32r(effective(cfg, "retract_length_toolchange", nxt))),
    );
    if cfg.get("filament_retract_length_nc").is_some() {
        let x = at(cfg, "filament_retract_length_nc", p).unwrap_or(0.0);
        set(
            &mut v,
            "filament_retract_length_nc",
            "Float",
            Value::Float(f32r(x)),
        );
    }
    miss(
        &mut v,
        "new_extruder_retracted_length",
        "writer retraction state",
    );

    // m_config holds these merged with the filament overrides
    let long: Vec<Option<bool>> = (0..n)
        .map(|i| Some(effective(cfg, "long_retractions_when_cut", i) != 0.0))
        .collect();
    let dist: Vec<Option<f64>> = (0..n)
        .map(|i| Some(effective(cfg, "retraction_distances_when_cut", i)))
        .collect();
    set(
        &mut v,
        "long_retraction_when_cut",
        "Bool",
        Value::Bool(long.get(p).copied().flatten().unwrap_or(false)),
    );
    set(
        &mut v,
        "retraction_distance_when_cut",
        "Float",
        Value::Float(dist.get(p).copied().flatten().unwrap_or(0.0)),
    );
    set(
        &mut v,
        "long_retractions_when_cut",
        "Bools",
        Value::Bools(long),
    );
    set(
        &mut v,
        "retraction_distances_when_cut",
        "Floats",
        Value::Floats(dist),
    );

    let fast = cfg.get("prime_volume_mode").and_then(Value::as_str) == Some("Fast");
    let mut speeds = Vec::new();
    let mut temps = Vec::new();
    for i in 0..n {
        let s = at(cfg, "filament_flush_volumetric_speed", i).unwrap_or(0.0);
        speeds.push(Some(if s != 0.0 {
            s
        } else {
            at(cfg, "filament_max_volumetric_speed", i).unwrap_or(0.0)
        }));
        let t = at(
            cfg,
            if fast {
                "filament_flush_temp_fast"
            } else {
                "filament_flush_temp"
            },
            i,
        )
        .unwrap_or(0.0);
        let t = if t != 0.0 {
            t
        } else {
            at(cfg, "nozzle_temperature_range_high", i).unwrap_or(0.0)
        };
        temps.push(Some(t as i64));
    }
    set(
        &mut v,
        "flush_volumetric_speeds",
        "Floats",
        Value::Floats(speeds),
    );
    set(&mut v, "flush_temperatures", "Ints", Value::Ints(temps));

    let mut flush_keys: Vec<String> = vec![
        "flush_length".into(),
        "first_flush_volume".into(),
        "second_flush_volume".into(),
    ];
    flush_keys.extend((1..=G_MAX_FLUSH_COUNT).map(|i| format!("flush_length_{i}")));
    let mut into: Vec<&str> = ["flush_into_infill", "flush_into_objects"]
        .into_iter()
        .filter(|k| truthy(cfg, k))
        .collect();
    if truthy(cfg, "flush_into_support") && truthy(cfg, "enable_support") {
        into.push("flush_into_support");
    }
    if c.wipe_tower && !into.is_empty() {
        // Bambu subtracts what it purges into objects/infill/support
        // (mark_wiping_extrusions), which depends on the geometry
        let why = format!("{} on: purge depends on object geometry", into.join(", "));
        for k in &flush_keys {
            miss(&mut v, k, &why);
        }
    } else {
        let (length, parts) = flush_lengths(cfg, p, nxt, c.wipe_tower);
        set(&mut v, "flush_length", "Float", Value::Float(length));
        for (i, part) in parts.iter().enumerate() {
            set(
                &mut v,
                &format!("flush_length_{}", i + 1),
                "Float",
                Value::Float(*part),
            );
        }
        set(
            &mut v,
            "first_flush_volume",
            "Float",
            Value::Float(f32r(length / 2.0)),
        );
        set(
            &mut v,
            "second_flush_volume",
            "Float",
            Value::Float(f32r(length / 2.0)),
        );
    }

    match travel_points(cfg, c.object_bboxes.as_deref()) {
        Ok(tp) => {
            for (i, (x, y)) in tp.iter().enumerate() {
                set(
                    &mut v,
                    &format!("travel_point_{}_x", i + 1),
                    "Float",
                    Value::Float(*x),
                );
                set(
                    &mut v,
                    &format!("travel_point_{}_y", i + 1),
                    "Float",
                    Value::Float(*y),
                );
            }
        }
        Err(e) => {
            for i in 1..=3 {
                miss(&mut v, &format!("travel_point_{i}_x"), &e.message);
                miss(&mut v, &format!("travel_point_{i}_y"), &e.message);
            }
        }
    }
    set(
        &mut v,
        "outer_wall_volumetric_speed",
        "Float",
        Value::Float(outer_wall_volumetric_speed(cfg, nxt)),
    );
    match c.position {
        Some((x, y, z)) => {
            for (k, val) in [("x", x), ("y", y), ("z", z)] {
                set(
                    &mut v,
                    &format!("{k}_after_toolchange"),
                    "Float",
                    Value::Float(f32r(val)),
                );
            }
        }
        None => {
            for k in ["x", "y", "z"] {
                miss(
                    &mut v,
                    &format!("{k}_after_toolchange"),
                    "nozzle position not given",
                );
            }
        }
    }
    v
}

// --------------------------------------------------------------------------- //
// reading Bambu's own tool changes from a sliced file
// --------------------------------------------------------------------------- //

#[derive(Debug, Clone, PartialEq)]
pub struct ToolChange {
    /// Index of the `T<next>` line.
    pub line: usize,
    pub prev: i64,
    pub next: usize,
    /// 1-based ordinal among the print's tool changes.
    pub count: i64,
    /// 0-based.
    pub layer_index: i64,
    pub layer_z: f64,
    pub max_layer_z: f64,
    pub in_wipe_tower: bool,
    /// Index of the layer's `; CHANGE_LAYER` line.
    pub layer_start: usize,
}

fn t_number(s: &str) -> Option<usize> {
    // ^T(\d+)\s*$
    let rest = s.strip_prefix('T')?.trim_end();
    (!rest.is_empty() && rest.bytes().all(|c| c.is_ascii_digit()))
        .then(|| rest.parse().ok())
        .flatten()
}

/// Tool changes Bambu Studio wrote into a sliced file: a bare `T<n>` line
/// (n < 255) after the first layer change. The filament loaded before the
/// first change is the one the start G-code loads (its first `M620 S<n>A`).
pub fn find_tool_changes<S: AsRef<str>>(lines: &[S]) -> Vec<ToolChange> {
    let mut out: Vec<ToolChange> = Vec::new();
    let mut current: Option<i64> = None;
    let (mut layer_idx, mut layer_z, mut max_z, mut layer_start) = (-1i64, 0.0f64, 0.0f64, 0usize);
    let mut in_tower = false;
    for (i, ln) in lines.iter().enumerate() {
        let s = ln.as_ref().trim();
        if current.is_none() {
            if let Some(rest) = s.strip_prefix("M620 S") {
                let d: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
                if !d.is_empty() && rest[d.len()..].starts_with('A') {
                    current = d.parse().ok();
                }
            }
        }
        if s == "; CHANGE_LAYER" {
            layer_idx += 1;
            layer_start = i;
            in_tower = false;
            continue;
        }
        if let Some(z) = s.strip_prefix("; Z_HEIGHT: ") {
            let num: String = z
                .chars()
                .take_while(|c| c.is_ascii_digit() || *c == '-' || *c == '.')
                .collect();
            if let Ok(z) = num.parse::<f64>() {
                layer_z = z;
                max_z = max_z.max(z);
                continue;
            }
        }
        if s.starts_with("; CP TOOLCHANGE START") {
            in_tower = true;
        } else if s.starts_with("; CP TOOLCHANGE END") {
            in_tower = false;
        }
        if layer_idx < 0 {
            continue;
        }
        if let Some(n) = t_number(s).filter(|&n| n < 255) {
            out.push(ToolChange {
                line: i,
                prev: current.unwrap_or(-1),
                next: n,
                count: out.len() as i64 + 1,
                layer_index: layer_idx,
                layer_z,
                max_layer_z: max_z,
                in_wipe_tower: in_tower,
                layer_start,
            });
            current = Some(n as i64);
        }
    }
    out
}

/// Each object's model bounding box, approximated from its outer walls
/// (grown by half a line width; arcs count by their end points only).
pub fn object_bboxes_from_gcode<S: AsRef<str>>(lines: &[S]) -> Vec<[f64; 4]> {
    let mut boxes: Vec<(String, [f64; 4])> = Vec::new();
    let (mut obj, mut feature, mut width): (Option<String>, Option<String>, f64) =
        (None, None, 0.0);
    let (mut x, mut y): (Option<f64>, Option<f64>) = (None, None);
    for ln in lines {
        let s = ln.as_ref().trim();
        if let Some(rest) = s.strip_prefix("; start printing object, unique label id:") {
            obj = Some(rest.trim().to_string());
        } else if s.starts_with("; stop printing object") {
            obj = None;
        } else if let Some(f) = s.strip_prefix("; FEATURE:") {
            feature = Some(f.trim().to_string());
        } else if let Some(w) = s.strip_prefix("; LINE_WIDTH:") {
            width = w.trim().parse().unwrap_or(width);
        } else if ["G0 ", "G1 ", "G2 ", "G3 "]
            .iter()
            .any(|p| s.starts_with(p))
        {
            let code = s.split(';').next().unwrap_or("");
            let (mut nx, mut ny, mut e) = (x, y, None);
            for w in code.split_whitespace().skip(1) {
                let (k, val) = w.split_at(1);
                match k {
                    "X" => nx = val.parse().ok().or(nx),
                    "Y" => ny = val.parse().ok().or(ny),
                    "E" => e = val.parse::<f64>().ok(),
                    _ => {}
                }
            }
            if let (Some(o), Some("Outer wall"), Some(e)) = (&obj, feature.as_deref(), e) {
                if e > 0.0 {
                    let i = match boxes.iter().position(|(k, _)| k == o) {
                        Some(i) => i,
                        None => {
                            boxes.push((
                                o.clone(),
                                [
                                    f64::INFINITY,
                                    f64::INFINITY,
                                    f64::NEG_INFINITY,
                                    f64::NEG_INFINITY,
                                ],
                            ));
                            boxes.len() - 1
                        }
                    };
                    let b = &mut boxes[i].1;
                    let h = width / 2.0;
                    for (px, py) in [(x, y), (nx, ny)] {
                        if let (Some(px), Some(py)) = (px, py) {
                            b[0] = b[0].min(px - h);
                            b[1] = b[1].min(py - h);
                            b[2] = b[2].max(px + h);
                            b[3] = b[3].max(py + h);
                        }
                    }
                }
            }
            x = nx;
            y = ny;
        }
    }
    boxes.into_iter().map(|(_, b)| b).collect()
}

// --------------------------------------------------------------------------- //
// Bambu's cooling pass: redundant feedrates
// --------------------------------------------------------------------------- //

/// What Bambu's GCodeEditor tracks while it rewrites a layer: the last F
/// (truncated to int, as `atoi`) seen in the layer, 0 at the layer start, and
/// X, Y, Z, E, to tell whether a line moves at all.
#[derive(Debug, Clone, PartialEq)]
pub struct FeedrateState {
    pub feedrate: i64,
    pub pos: [f64; 4],
    pub relative_e: bool,
}

impl FeedrateState {
    pub fn new(relative_e: bool) -> Self {
        FeedrateState {
            feedrate: 0,
            pos: [0.0; 4],
            relative_e,
        }
    }
}

fn parse_move(s: &str) -> (String, HashMap<char, f64>) {
    let code = s.split(';').next().unwrap_or("");
    let mut words = code.split_whitespace();
    let cmd = words.next().unwrap_or("").to_string();
    let mut vals = HashMap::new();
    for w in words {
        let mut cs = w.chars();
        if let Some(k) = cs.next().filter(|k| "XYZEFIJ".contains(*k)) {
            if let Ok(v) = cs.as_str().parse::<f64>() {
                vals.insert(k, v);
            }
        }
    }
    (cmd, vals)
}

/// The integer after ` F` in a line (as `atoi`), if any.
fn f_word(s: &str) -> Option<i64> {
    let i = s.find(" F")?;
    let rest = &s[i + 2..];
    let num: String = rest
        .chars()
        .enumerate()
        .take_while(|(k, c)| c.is_ascii_digit() || (*k == 0 && *c == '-'))
        .map(|(_, c)| c)
        .collect();
    num.parse().ok()
}

/// Apply GCodeEditor's F-word rule to lines Bambu passes through its cooling
/// pass: a G0/G1/G2/G3 line whose F equals the current feedrate loses the F
/// word; if it does not move at all it is dropped. `state` is updated.
pub fn drop_redundant_feedrates<S: AsRef<str>>(
    lines: &[S],
    state: &mut FeedrateState,
) -> Vec<String> {
    let mut out = Vec::new();
    for ln in lines {
        let s = ln.as_ref().trim_end_matches('\n');
        // GCodeEditor matches the raw line start: indented moves are left alone
        if !["G0 ", "G1 ", "G2 ", "G3 ", "G92 "]
            .iter()
            .any(|p| s.starts_with(p))
        {
            out.push(s.to_string());
            continue;
        }
        let (cmd, vals) = parse_move(s);
        let mut new = state.pos;
        for (ax, k) in [('X', 0), ('Y', 1), ('Z', 2), ('E', 3)] {
            if let Some(v) = vals.get(&ax) {
                new[k] = *v;
            }
        }
        if cmd == "G92" {
            state.pos = new;
            out.push(s.to_string());
            continue;
        }
        if state.relative_e {
            state.pos[3] = 0.0;
        }
        let d: Vec<f64> = (0..4).map(|k| new[k] - state.pos[k]).collect();
        let length = if cmd == "G2" || cmd == "G3" {
            if vals.contains_key(&'I') || vals.contains_key(&'J') {
                1.0
            } else {
                d[0].hypot(d[1])
            }
        } else {
            let l = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt();
            if l != 0.0 {
                l
            } else {
                d[3].abs()
            }
        };
        state.pos = new;
        let mut keep = s.to_string();
        if let Some(fv) = vals.get(&'F') {
            let f = f_word(s).unwrap_or(*fv as i64);
            if f == state.feedrate {
                if length == 0.0 {
                    continue; // whole line dropped
                }
                let (code, comment) = match s.find(';') {
                    Some(i) => (&s[..i], Some(&s[i..])),
                    None => (s, None),
                };
                let code = remove_f_word(code);
                if code.trim() == "G1" || code.trim() == "G0" {
                    continue;
                }
                keep = code + comment.unwrap_or("");
            } else {
                state.feedrate = f;
            }
        }
        out.push(keep);
    }
    out
}

/// `[ \t]+F[^ \t;]*` removed once.
fn remove_f_word(code: &str) -> String {
    let b = code.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b' ' || b[i] == b'\t' {
            let mut j = i;
            while j < b.len() && (b[j] == b' ' || b[j] == b'\t') {
                j += 1;
            }
            if j < b.len() && b[j] == b'F' {
                let mut k = j + 1;
                while k < b.len() && !matches!(b[k], b' ' | b'\t' | b';') {
                    k += 1;
                }
                return format!("{}{}", &code[..i], &code[k..]);
            }
            i = j;
        } else {
            i += 1;
        }
    }
    code.to_string()
}

/// GCodeEditor's state just before `lines[index]` (positions from the file
/// start, F from the layer start).
pub fn feedrate_state_before<S: AsRef<str>>(
    lines: &[S],
    index: usize,
    layer_start: usize,
    relative_e: bool,
) -> FeedrateState {
    let mut st = FeedrateState::new(relative_e);
    drop_redundant_feedrates(&lines[..layer_start], &mut st);
    st.feedrate = 0; // Bambu runs the pass per layer
    drop_redundant_feedrates(&lines[layer_start..index], &mut st);
    st
}

/// A G-code line without its comment and with single spaces; None if nothing is left.
pub fn meaningful(line: &str) -> Option<String> {
    let code = line.split(';').next().unwrap_or("").trim();
    if code.is_empty() {
        return None;
    }
    Some(code.split_whitespace().collect::<Vec<_>>().join(" "))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vars() -> Vars {
        let mut v = Vars::new();
        set(&mut v, "i", "Int", Value::Int(7));
        set(&mut v, "f", "Float", Value::Float(2.5));
        set(&mut v, "b", "Bool", Value::Bool(true));
        set(&mut v, "s", "String", Value::Str("PETG".into()));
        set(
            &mut v,
            "fs",
            "Floats",
            Value::Floats(vec![Some(21.0), Some(12.5)]),
        );
        set(
            &mut v,
            "is_",
            "Ints",
            Value::Ints(vec![Some(220), Some(260)]),
        );
        set(
            &mut v,
            "bs",
            "Bools",
            Value::Bools(vec![Some(false), Some(true)]),
        );
        set(
            &mut v,
            "ss",
            "Strings",
            Value::Strs(vec!["PLA".into(), "PETG".into()]),
        );
        set(&mut v, "n", "Int", Value::Int(1));
        set(&mut v, "neg", "Int", Value::Int(-1));
        v
    }

    fn x(src: &str) -> String {
        expand(src, &vars(), None, 1).unwrap()
    }

    #[test]
    fn expressions() {
        for (src, out) in [
            ("{i / 2}", "3"),
            ("{-i / 2}", "-3"),
            ("{i % 3}", "1"),
            ("{-i % 3}", "-1"),
            ("{f * 2}", "5"),
            ("{i / 2.0}", "3.5"),
            ("{fs[1]}", "12.5"),
            ("{fs[n]}", "12.5"),
            ("{fs[9]}", "21"),
            ("{is_[0] + 1}", "221"),
            ("{ss[1] + \"!\"}", "PETG!"),
            ("{min(i, 3)}", "3"),
            ("{max(f, 3)}", "3"),
            ("{int(2.9)}", "2"),
            ("{round(-2.5)}", "-3"),
            ("{floor(-2.5)}", "-3"),
            ("{ceil(2.1)}", "3"),
            ("{1/3.0}", "0.333333"),
            ("{100000000.0}", "1e+08"),
            ("[is_]", "260"),
            ("[is__0]", "220"),
            ("[is_[n]]", "260"),
            ("[f]", "2.5"),
        ] {
            assert_eq!(x(src), out, "{src}");
        }
    }

    #[test]
    fn conditions_and_nesting() {
        assert_eq!(x("{if i > 5}a{elsif i > 2}b{else}c{endif}"), "a");
        assert_eq!(x("{if i > 9}a{elsif i > 2}b{else}c{endif}"), "b");
        assert_eq!(x("{if s == \"PETG\" && !bs[0]}y{endif}"), "y");
        assert_eq!(x("{if b}{if i == 7}A{else}B{endif}{endif}"), "A");
        assert_eq!(x("{if 2.0 == 2}t{endif}"), "t");
        assert_eq!(x("{if (b ? 1 : 2) == 1}t{endif}"), "t");
    }

    #[test]
    fn errors() {
        for src in [
            "{if i}x{endif}",
            "{i && b}",
            "{nope}",
            "[nope]",
            "{if b}x",
            "{endif}",
            "{1/0}",
        ] {
            assert!(expand(src, &vars(), None, 0).is_err(), "{src}");
        }
        let mut v = vars();
        miss(&mut v, "m", "why");
        let e = expand("{m}", &v, None, 0).unwrap_err();
        assert_eq!(e.missing.as_deref(), Some("m"));
    }

    #[test]
    fn feedrate_pass() {
        let mut st = FeedrateState::new(true);
        let out = drop_redundant_feedrates(
            &["G1 X1 F600", "G1 X2 F600", "G1 F600", "G1 X3 F900 ; c"],
            &mut st,
        );
        assert_eq!(out, ["G1 X1 F600", "G1 X2", "G1 X3 F900 ; c"]);
    }
}
