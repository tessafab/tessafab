//! Parameter expressions in user patterns: `{{ plank_width * ratio }}` inside an SVG.
//!
//! Only arithmetic is allowed: numbers, strings, the pattern's parameters,
//! `+ - * / // % **`, comparisons, `not`/`and`/`or`, `a if cond else b`, and a
//! few maths functions and constants. Nothing else, so a pattern file can't run
//! code. The syntax and results follow the Python engine (expressions.py),
//! whose patterns are written in Python's expression syntax: integers stay
//! integers, `/` gives a float, `//` and `%` floor, `round` rounds half to even.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

pub const MAX_LENGTH: usize = 400;
pub const FUNCTIONS: [&str; 11] = [
    "sqrt", "sin", "cos", "tan", "abs", "min", "max", "round", "floor", "ceil", "int",
];
pub const CONSTANTS: [&str; 5] = ["pi", "sqrt2", "sqrt3", "true", "false"];

#[derive(Debug, Clone, PartialEq)]
pub struct Error(pub String);

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Error {}

/// A value: a parameter or an expression's result.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Int(i64),
    Float(f64),
    Bool(bool),
    Str(String),
}

impl Value {
    /// The value as JSON (for parameter values from a form).
    pub fn from_json(v: &serde_json::Value) -> Option<Value> {
        match v {
            serde_json::Value::Bool(b) => Some(Value::Bool(*b)),
            serde_json::Value::Number(n) => Some(match n.as_i64() {
                Some(i) => Value::Int(i),
                None => Value::Float(n.as_f64()?),
            }),
            serde_json::Value::String(s) => Some(Value::Str(s.clone())),
            _ => None,
        }
    }

    fn type_name(&self) -> &'static str {
        match self {
            Value::Int(_) => "int",
            Value::Float(_) => "float",
            Value::Bool(_) => "bool",
            Value::Str(_) => "str",
        }
    }

    fn truthy(&self) -> bool {
        match self {
            Value::Int(i) => *i != 0,
            Value::Float(f) => *f != 0.0,
            Value::Bool(b) => *b,
            Value::Str(s) => !s.is_empty(),
        }
    }

    /// Numbers as Python sees them: bool is an int.
    fn num(&self) -> Option<Num> {
        match self {
            Value::Int(i) => Some(Num::I(*i)),
            Value::Bool(b) => Some(Num::I(*b as i64)),
            Value::Float(f) => Some(Num::F(*f)),
            Value::Str(_) => None,
        }
    }

    pub fn as_f64(&self) -> Option<f64> {
        self.num().map(|n| n.f())
    }
}

#[derive(Clone, Copy)]
enum Num {
    I(i64),
    F(f64),
}

impl Num {
    fn f(self) -> f64 {
        match self {
            Num::I(i) => i as f64,
            Num::F(f) => f,
        }
    }
}

pub type Values = BTreeMap<String, Value>;

// --------------------------------------------------------------------------- //
// Tokens and syntax
// --------------------------------------------------------------------------- //

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Int(i64),
    Float(f64),
    Str(String),
    Name(String),
    Op(&'static str),
}

const OPS: [&str; 18] = [
    "**", "//", "==", "!=", "<=", ">=", "+", "-", "*", "/", "%", "<", ">", "(", ")", ",", "[", ".",
];

fn tokenize(src: &str) -> Option<Vec<Tok>> {
    let b = src.as_bytes();
    let mut i = 0;
    let mut out = Vec::new();
    while i < b.len() {
        let c = b[i];
        if c.is_ascii_whitespace() {
            i += 1;
        } else if c.is_ascii_digit() || (c == b'.' && i + 1 < b.len() && b[i + 1].is_ascii_digit())
        {
            let start = i;
            let mut float = false;
            while i < b.len() && (b[i].is_ascii_digit() || b[i] == b'_') {
                i += 1;
            }
            if i < b.len() && b[i] == b'.' {
                float = true;
                i += 1;
                while i < b.len() && (b[i].is_ascii_digit() || b[i] == b'_') {
                    i += 1;
                }
            }
            if i < b.len() && (b[i] == b'e' || b[i] == b'E') {
                let mut j = i + 1;
                if j < b.len() && (b[j] == b'+' || b[j] == b'-') {
                    j += 1;
                }
                if j < b.len() && b[j].is_ascii_digit() {
                    float = true;
                    i = j;
                    while i < b.len() && b[i].is_ascii_digit() {
                        i += 1;
                    }
                }
            }
            let text: String = src[start..i].chars().filter(|&c| c != '_').collect();
            if i < b.len() && (b[i].is_ascii_alphabetic() || b[i] == b'_') {
                return None; // 1abc, 0x10 ...
            }
            out.push(if float {
                Tok::Float(text.parse().ok()?)
            } else {
                Tok::Int(text.parse().ok()?)
            });
        } else if c.is_ascii_alphabetic() || c == b'_' {
            let start = i;
            while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_') {
                i += 1;
            }
            out.push(Tok::Name(src[start..i].to_string()));
        } else if c == b'\'' || c == b'"' {
            let start = i + 1;
            i += 1;
            while i < b.len() && b[i] != c {
                if b[i] == b'\\' {
                    return None;
                }
                i += 1;
            }
            if i >= b.len() {
                return None;
            }
            out.push(Tok::Str(src[start..i].to_string()));
            i += 1;
        } else {
            let op = OPS.iter().find(|op| src[i..].starts_with(*op))?;
            out.push(Tok::Op(op));
            i += op.len();
        }
    }
    Some(out)
}

#[derive(Debug, Clone)]
enum Node {
    Int(i64),
    Float(f64),
    Str(String),
    Name(String),
    /// A syntax Python allows but expressions don't (subscripts, attributes, keywords ...).
    Forbidden,
    Unary(&'static str, Box<Node>),
    Binary(&'static str, Box<Node>, Box<Node>),
    Compare(Box<Node>, Vec<(&'static str, Node)>),
    And(Vec<Node>),
    Or(Vec<Node>),
    IfElse(Box<Node>, Box<Node>, Box<Node>),
    Call(String, Vec<Node>),
}

struct Parser {
    toks: Vec<Tok>,
    pos: usize,
}

type P = Result<Node, ()>;

impl Parser {
    fn peek(&self) -> Option<&Tok> {
        self.toks.get(self.pos)
    }

    fn is_op(&self, op: &str) -> bool {
        matches!(self.peek(), Some(Tok::Op(o)) if *o == op)
    }

    fn is_word(&self, w: &str) -> bool {
        matches!(self.peek(), Some(Tok::Name(n)) if n == w)
    }

    fn eat_op(&mut self, op: &str) -> bool {
        let yes = self.is_op(op);
        self.pos += yes as usize;
        yes
    }

    fn eat_word(&mut self, w: &str) -> bool {
        let yes = self.is_word(w);
        self.pos += yes as usize;
        yes
    }

    // test: or_test ['if' or_test 'else' test]
    fn test(&mut self) -> P {
        if self.is_word("lambda") {
            return Err(());
        }
        let body = self.or_test()?;
        if self.eat_word("if") {
            let cond = self.or_test()?;
            if !self.eat_word("else") {
                return Err(());
            }
            let other = self.test()?;
            return Ok(Node::IfElse(
                Box::new(cond),
                Box::new(body),
                Box::new(other),
            ));
        }
        Ok(body)
    }

    fn or_test(&mut self) -> P {
        let mut v = vec![self.and_test()?];
        while self.eat_word("or") {
            v.push(self.and_test()?);
        }
        Ok(if v.len() == 1 {
            v.pop().unwrap()
        } else {
            Node::Or(v)
        })
    }

    fn and_test(&mut self) -> P {
        let mut v = vec![self.not_test()?];
        while self.eat_word("and") {
            v.push(self.not_test()?);
        }
        Ok(if v.len() == 1 {
            v.pop().unwrap()
        } else {
            Node::And(v)
        })
    }

    fn not_test(&mut self) -> P {
        if self.eat_word("not") {
            return Ok(Node::Unary("not", Box::new(self.not_test()?)));
        }
        self.comparison()
    }

    fn comparison(&mut self) -> P {
        let left = self.arith()?;
        let mut rest = Vec::new();
        loop {
            let op = match self.peek() {
                Some(Tok::Op(o)) if ["==", "!=", "<", "<=", ">", ">="].contains(o) => *o,
                Some(Tok::Name(n)) if n == "in" || n == "is" => return Ok(Node::Forbidden),
                _ => break,
            };
            self.pos += 1;
            rest.push((op, self.arith()?));
        }
        Ok(if rest.is_empty() {
            left
        } else {
            Node::Compare(Box::new(left), rest)
        })
    }

    fn arith(&mut self) -> P {
        let mut left = self.term()?;
        loop {
            let op = if self.eat_op("+") {
                "+"
            } else if self.eat_op("-") {
                "-"
            } else {
                break;
            };
            left = Node::Binary(op, Box::new(left), Box::new(self.term()?));
        }
        Ok(left)
    }

    fn term(&mut self) -> P {
        let mut left = self.factor()?;
        loop {
            let op = ["*", "/", "//", "%"].into_iter().find(|op| self.is_op(op));
            let Some(op) = op else { break };
            self.pos += 1;
            left = Node::Binary(op, Box::new(left), Box::new(self.factor()?));
        }
        Ok(left)
    }

    fn factor(&mut self) -> P {
        for op in ["+", "-"] {
            if self.eat_op(op) {
                return Ok(Node::Unary(op, Box::new(self.factor()?)));
            }
        }
        self.power()
    }

    fn power(&mut self) -> P {
        let base = self.atom()?;
        if self.eat_op("**") {
            return Ok(Node::Binary("**", Box::new(base), Box::new(self.factor()?)));
        }
        Ok(base)
    }

    fn atom(&mut self) -> P {
        let t = self.peek().cloned().ok_or(())?;
        self.pos += 1;
        let node = match t {
            Tok::Int(i) => Node::Int(i),
            Tok::Float(f) => Node::Float(f),
            Tok::Str(s) => {
                let mut s = s;
                while let Some(Tok::Str(more)) = self.peek().cloned() {
                    self.pos += 1;
                    s.push_str(&more); // 'a' 'b' joins, as in Python
                }
                Node::Str(s)
            }
            Tok::Name(n) => {
                if ["if", "else", "and", "or", "not", "in", "is", "lambda"].contains(&n.as_str()) {
                    return Err(());
                }
                if self.eat_op("(") {
                    let mut args = Vec::new();
                    let mut forbidden = false;
                    if !self.eat_op(")") {
                        loop {
                            // keyword arguments (name=...) aren't allowed
                            if matches!(self.peek(), Some(Tok::Name(_)))
                                && matches!(self.toks.get(self.pos + 1), Some(Tok::Op("=")))
                            {
                                forbidden = true;
                            }
                            args.push(self.test()?);
                            if self.eat_op(")") {
                                break;
                            }
                            if !self.eat_op(",") {
                                return Err(());
                            }
                            if self.eat_op(")") {
                                break;
                            }
                        }
                    }
                    if forbidden {
                        Node::Forbidden
                    } else {
                        Node::Call(n, args)
                    }
                } else {
                    Node::Name(n)
                }
            }
            Tok::Op("(") => {
                let inner = self.test()?;
                if self.eat_op(",") {
                    return Ok(Node::Forbidden); // a tuple
                }
                if !self.eat_op(")") {
                    return Err(());
                }
                inner
            }
            Tok::Op("[") => return Ok(Node::Forbidden),
            Tok::Op(_) => return Err(()),
        };
        // subscripts and attributes parse in Python but aren't arithmetic
        if self.is_op("[") || self.is_op(".") || self.is_op("(") {
            return Ok(Node::Forbidden);
        }
        Ok(node)
    }
}

fn parse(text: &str) -> Result<Node, ()> {
    let toks = tokenize(text).ok_or(())?;
    let mut p = Parser { toks, pos: 0 };
    let node = p.test()?;
    if p.pos != p.toks.len() {
        // what's left may be syntax Python accepts (a[0], a.b, f(x)(y)); either way it's refused
        return if contains_forbidden(&node)
            || p.toks[p.pos..]
                .iter()
                .any(|t| matches!(t, Tok::Op("[" | ".")))
        {
            Ok(Node::Forbidden)
        } else {
            Err(())
        };
    }
    Ok(node)
}

fn contains_forbidden(n: &Node) -> bool {
    match n {
        Node::Forbidden => true,
        Node::Unary(_, a) => contains_forbidden(a),
        Node::Binary(_, a, b) => contains_forbidden(a) || contains_forbidden(b),
        Node::Compare(a, r) => {
            contains_forbidden(a) || r.iter().any(|(_, b)| contains_forbidden(b))
        }
        Node::And(v) | Node::Or(v) | Node::Call(_, v) => v.iter().any(contains_forbidden),
        Node::IfElse(a, b, c) => {
            contains_forbidden(a) || contains_forbidden(b) || contains_forbidden(c)
        }
        _ => false,
    }
}

// --------------------------------------------------------------------------- //
// Evaluation
// --------------------------------------------------------------------------- //

struct Eval<'a> {
    values: &'a Values,
    text: &'a str,
}

/// Python's floor division and modulo.
fn floordiv_i(a: i64, b: i64) -> i64 {
    let q = a / b;
    if (a % b != 0) && ((a < 0) != (b < 0)) {
        q - 1
    } else {
        q
    }
}

fn pymod_f(a: f64, b: f64) -> f64 {
    let m = a % b;
    if m == 0.0 {
        0.0f64.copysign(b) // Python: a zero remainder takes the divisor's sign
    } else if (m < 0.0) != (b < 0.0) {
        m + b
    } else {
        m
    }
}

/// Python's float floor division (CPython float_divmod): exact where floor(a / b)
/// would round the quotient first (10 // 0.4 is 24, not 25).
fn floordiv_f(a: f64, b: f64) -> f64 {
    let m = a % b;
    let mut div = (a - m) / b;
    if m != 0.0 && (b < 0.0) != (m < 0.0) {
        div -= 1.0;
    }
    if div != 0.0 {
        let mut fd = div.floor();
        if div - fd > 0.5 {
            fd += 1.0;
        }
        fd
    } else {
        0.0f64.copysign(a / b)
    }
}

fn round_half_even(x: f64) -> f64 {
    let r = x.round();
    if (x - x.trunc()).abs() == 0.5 {
        2.0 * (x / 2.0).round()
    } else {
        r
    }
}

fn to_int(f: f64, what: &str) -> Result<i64, String> {
    if !f.is_finite() {
        return Err(format!(
            "cannot convert float {} to integer",
            if f.is_nan() { "NaN" } else { "infinity" }
        ));
    }
    if f.abs() >= 9.2e18 {
        return Err(format!("{what}: number too large"));
    }
    Ok(f as i64)
}

impl Eval<'_> {
    fn fail<T>(&self, msg: impl fmt::Display) -> Result<T, Error> {
        Err(Error(format!("{{{{ {} }}}}: {}", self.text, msg)))
    }

    fn only_arithmetic<T>(&self) -> Result<T, Error> {
        Err(Error(format!(
            "only arithmetic is allowed in {{{{ {} }}}}",
            self.text
        )))
    }

    fn ev(&self, n: &Node) -> Result<Value, Error> {
        match n {
            Node::Int(i) => Ok(Value::Int(*i)),
            Node::Float(f) => Ok(Value::Float(*f)),
            Node::Str(s) => Ok(Value::Str(s.clone())),
            Node::Forbidden => self.only_arithmetic(),
            Node::Name(name) => {
                if let Some(v) = self.values.get(name) {
                    return Ok(v.clone());
                }
                match name.as_str() {
                    "pi" => Ok(Value::Float(std::f64::consts::PI)),
                    "sqrt2" => Ok(Value::Float(2f64.sqrt())),
                    "sqrt3" => Ok(Value::Float(3f64.sqrt())),
                    "true" => Ok(Value::Bool(true)),
                    "false" => Ok(Value::Bool(false)),
                    // Python's own True / False / None aren't numbers or parameters here
                    "True" | "False" | "None" => self.only_arithmetic(),
                    _ => Err(Error(format!(
                        "unknown name '{}' in {{{{ {} }}}}",
                        name, self.text
                    ))),
                }
            }
            Node::Unary(op, a) => {
                let v = self.ev(a)?;
                match (*op, v.num()) {
                    ("not", _) => Ok(Value::Bool(!v.truthy())),
                    ("-", Some(Num::I(i))) => i
                        .checked_neg()
                        .map(Value::Int)
                        .map_or_else(|| self.fail("integer overflow"), Ok),
                    ("-", Some(Num::F(f))) => Ok(Value::Float(-f)),
                    ("+", Some(Num::I(i))) => Ok(Value::Int(i)),
                    ("+", Some(Num::F(f))) => Ok(Value::Float(f)),
                    _ => self.fail(format!(
                        "bad operand type for unary {}: '{}'",
                        op,
                        v.type_name()
                    )),
                }
            }
            Node::Binary(op, a, b) => {
                let (x, y) = (self.ev(a)?, self.ev(b)?);
                self.binary(op, x, y)
            }
            Node::Compare(first, rest) => {
                let mut left = self.ev(first)?;
                for (op, rn) in rest {
                    let right = self.ev(rn)?;
                    if !self.compare(op, &left, &right)? {
                        return Ok(Value::Bool(false));
                    }
                    left = right;
                }
                Ok(Value::Bool(true))
            }
            Node::And(v) => {
                let mut all = true;
                for n in v {
                    all &= self.ev(n)?.truthy();
                }
                Ok(Value::Bool(all))
            }
            Node::Or(v) => {
                let mut any = false;
                for n in v {
                    any |= self.ev(n)?.truthy();
                }
                Ok(Value::Bool(any))
            }
            Node::IfElse(cond, body, other) => {
                if self.ev(cond)?.truthy() {
                    self.ev(body)
                } else {
                    self.ev(other)
                }
            }
            Node::Call(name, args) => {
                if !FUNCTIONS.contains(&name.as_str()) {
                    return self.only_arithmetic();
                }
                let args: Vec<Value> = args.iter().map(|a| self.ev(a)).collect::<Result<_, _>>()?;
                self.call(name, args)
            }
        }
    }

    fn binary(&self, op: &str, x: Value, y: Value) -> Result<Value, Error> {
        if let (Value::Str(a), Value::Str(b)) = (&x, &y) {
            if op == "+" {
                return Ok(Value::Str(format!("{a}{b}")));
            }
        }
        let (Some(a), Some(b)) = (x.num(), y.num()) else {
            return self.fail(format!(
                "unsupported operand type(s) for {}: '{}' and '{}'",
                op,
                x.type_name(),
                y.type_name()
            ));
        };
        let overflow = || Error(format!("{{{{ {} }}}}: integer overflow", self.text));
        Ok(match (op, a, b) {
            ("+", Num::I(p), Num::I(q)) => Value::Int(p.checked_add(q).ok_or_else(overflow)?),
            ("-", Num::I(p), Num::I(q)) => Value::Int(p.checked_sub(q).ok_or_else(overflow)?),
            ("*", Num::I(p), Num::I(q)) => Value::Int(p.checked_mul(q).ok_or_else(overflow)?),
            ("+", p, q) => Value::Float(p.f() + q.f()),
            ("-", p, q) => Value::Float(p.f() - q.f()),
            ("*", p, q) => Value::Float(p.f() * q.f()),
            ("/", p, q) => {
                if q.f() == 0.0 {
                    return self.fail("division by zero");
                }
                Value::Float(p.f() / q.f())
            }
            ("//", Num::I(p), Num::I(q)) => {
                if q == 0 {
                    return self.fail("integer division or modulo by zero");
                }
                Value::Int(floordiv_i(p, q))
            }
            ("//", p, q) => {
                if q.f() == 0.0 {
                    return self.fail("float floor division by zero");
                }
                Value::Float(floordiv_f(p.f(), q.f()))
            }
            ("%", Num::I(p), Num::I(q)) => {
                if q == 0 {
                    return self.fail("integer modulo by zero");
                }
                Value::Int(p - q * floordiv_i(p, q))
            }
            ("%", p, q) => {
                if q.f() == 0.0 {
                    return self.fail("float modulo");
                }
                Value::Float(pymod_f(p.f(), q.f()))
            }
            ("**", p, q) => {
                if q.f().abs() > 64.0 {
                    return Err(Error(format!("power too large in {{{{ {} }}}}", self.text)));
                }
                match (p, q) {
                    (Num::I(p), Num::I(q)) if q >= 0 => {
                        Value::Int(p.checked_pow(q as u32).ok_or_else(overflow)?)
                    }
                    (p, q) => {
                        if p.f() == 0.0 && q.f() < 0.0 {
                            return self.fail("0.0 cannot be raised to a negative power");
                        }
                        let r = p.f().powf(q.f());
                        if r.is_nan() {
                            return self
                                .fail("negative number cannot be raised to a fractional power");
                        }
                        Value::Float(r)
                    }
                }
            }
            _ => return self.only_arithmetic(),
        })
    }

    fn compare(&self, op: &str, a: &Value, b: &Value) -> Result<bool, Error> {
        if let (Some(x), Some(y)) = (a.num(), b.num()) {
            let (x, y) = match (x, y) {
                (Num::I(p), Num::I(q)) => (p as f64, q as f64),
                (p, q) => (p.f(), q.f()),
            };
            return Ok(match op {
                "==" => x == y,
                "!=" => x != y,
                "<" => x < y,
                "<=" => x <= y,
                ">" => x > y,
                _ => x >= y,
            });
        }
        match (a, b) {
            (Value::Str(x), Value::Str(y)) => Ok(match op {
                "==" => x == y,
                "!=" => x != y,
                "<" => x < y,
                "<=" => x <= y,
                ">" => x > y,
                _ => x >= y,
            }),
            _ => match op {
                "==" => Ok(false),
                "!=" => Ok(true),
                _ => self.fail(format!(
                    "'{}' not supported between instances of '{}' and '{}'",
                    op,
                    a.type_name(),
                    b.type_name()
                )),
            },
        }
    }

    fn call(&self, name: &str, args: Vec<Value>) -> Result<Value, Error> {
        let num = |v: &Value| -> Result<Num, Error> {
            v.num().ok_or_else(|| {
                Error(format!(
                    "{{{{ {} }}}}: {}() needs a number",
                    self.text, name
                ))
            })
        };
        let one = |args: &Vec<Value>| -> Result<Num, Error> {
            if args.len() != 1 {
                return self.fail(format!(
                    "{name}() takes exactly one argument ({} given)",
                    args.len()
                ));
            }
            num(&args[0])
        };
        let float = |f: f64| -> Result<Value, Error> { Ok(Value::Float(f)) };
        match name {
            "sqrt" => {
                let x = one(&args)?.f();
                if x < 0.0 {
                    return self.fail("math domain error");
                }
                float(x.sqrt())
            }
            "sin" => float(one(&args)?.f().to_radians().sin()),
            "cos" => float(one(&args)?.f().to_radians().cos()),
            "tan" => float(one(&args)?.f().to_radians().tan()),
            "abs" => match one(&args)? {
                Num::I(i) => i
                    .checked_abs()
                    .map(Value::Int)
                    .ok_or_else(|| Error("integer overflow".into())),
                Num::F(f) => float(f.abs()),
            },
            "floor" | "ceil" => match one(&args)? {
                Num::I(i) => Ok(Value::Int(i)),
                Num::F(f) => {
                    let r = if name == "floor" { f.floor() } else { f.ceil() };
                    to_int(r, name).map(Value::Int).or_else(|e| self.fail(e))
                }
            },
            "int" => {
                if args.len() != 1 {
                    return self.fail(format!("int() takes one argument ({} given)", args.len()));
                }
                match &args[0] {
                    Value::Str(s) => match s.trim().parse::<i64>() {
                        Ok(i) => Ok(Value::Int(i)),
                        Err(_) => {
                            self.fail(format!("invalid literal for int() with base 10: '{s}'"))
                        }
                    },
                    v => match num(v)? {
                        Num::I(i) => Ok(Value::Int(i)),
                        Num::F(f) => to_int(f.trunc(), name)
                            .map(Value::Int)
                            .or_else(|e| self.fail(e)),
                    },
                }
            }
            "round" => match args.len() {
                1 => match num(&args[0])? {
                    Num::I(i) => Ok(Value::Int(i)),
                    Num::F(f) => to_int(round_half_even(f), name)
                        .map(Value::Int)
                        .or_else(|e| self.fail(e)),
                },
                2 => {
                    let nd = match num(&args[1])? {
                        Num::I(i) => i,
                        Num::F(_) => {
                            return self.fail("'float' object cannot be interpreted as an integer")
                        }
                    };
                    match num(&args[0])? {
                        Num::I(i) if nd >= 0 => Ok(Value::Int(i)),
                        Num::I(i) => {
                            let p = 10f64.powi((-nd) as i32);
                            Ok(Value::Int((round_half_even(i as f64 / p) * p) as i64))
                        }
                        Num::F(f) => {
                            // Python rounds the exact decimal value; formatting does the same
                            if !(-20..=20).contains(&nd) {
                                return float(if nd > 0 { f } else { 0.0 });
                            }
                            if nd >= 0 {
                                float(format!("{:.*}", nd as usize, f).parse().unwrap_or(f))
                            } else {
                                let p = 10f64.powi((-nd) as i32);
                                float(round_half_even(f / p) * p)
                            }
                        }
                    }
                }
                n => self.fail(format!("round() takes at most 2 arguments ({n} given)")),
            },
            "min" | "max" => {
                if args.len() < 2 {
                    return self.fail(format!("{name}() needs at least two numbers here"));
                }
                let mut best = args[0].clone();
                for v in args.into_iter().skip(1) {
                    let better = if name == "min" {
                        self.compare("<", &v, &best)?
                    } else {
                        self.compare(">", &v, &best)?
                    };
                    if better {
                        best = v;
                    }
                }
                Ok(best)
            }
            _ => self.only_arithmetic(),
        }
    }
}

/// The value of one expression with the parameters in `values`.
pub fn evaluate(text: &str, values: &Values) -> Result<Value, Error> {
    let text = text.trim();
    if text.is_empty() {
        return Err(Error("empty expression {{ }}".into()));
    }
    if text.chars().count() > MAX_LENGTH {
        let head: String = text.chars().take(40).collect();
        return Err(Error(format!("expression too long: {head}...")));
    }
    let node = parse(text).map_err(|_| Error(format!("can't read {{{{ {text} }}}}")))?;
    Eval { values, text }.ev(&node)
}

/// A value as written into the SVG: up to 4 decimals, no trailing zeros.
pub fn format_value(v: &Value) -> Result<String, Error> {
    let x = match v {
        Value::Bool(b) => return Ok(if *b { "1" } else { "0" }.into()),
        Value::Str(s) => return Ok(s.clone()),
        Value::Int(i) => *i as f64,
        Value::Float(f) => *f,
    };
    if !x.is_finite() {
        return Err(Error(format!(
            "an expression gave {}",
            if x.is_nan() {
                "nan"
            } else if x > 0.0 {
                "inf"
            } else {
                "-inf"
            }
        )));
    }
    let s = match v {
        Value::Int(i) => format!("{i}.0000"),
        _ => format!("{x:.4}"),
    };
    Ok(s.trim_end_matches('0').trim_end_matches('.').to_string())
}

/// Split text into (is_comment, part) pieces around XML comments `<!-- ... -->`.
fn split_comments(text: &str) -> Vec<(bool, &str)> {
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(a) = rest.find("<!--") {
        match rest[a + 4..].find("-->") {
            Some(b) => {
                let end = a + 4 + b + 3;
                out.push((false, &rest[..a]));
                out.push((true, &rest[a..end]));
                rest = &rest[end..];
            }
            None => break,
        }
    }
    out.push((false, rest));
    out
}

/// Each `{{ ... }}` in `text`: (start, end, inner text).
fn placeholders(text: &str) -> Vec<(usize, usize, &str)> {
    let mut out = Vec::new();
    let mut i = 0;
    while let Some(a) = text[i..].find("{{") {
        let a = i + a;
        match text[a + 2..].find("}}") {
            Some(b) => {
                let end = a + 2 + b + 2;
                out.push((a, end, &text[a + 2..a + 2 + b]));
                i = end;
            }
            None => break,
        }
    }
    out
}

/// `text` with every `{{ expression }}` replaced by its value (XML comments are left alone).
pub fn substitute(text: &str, values: &Values) -> Result<String, Error> {
    let mut out = String::with_capacity(text.len());
    for (comment, part) in split_comments(text) {
        if comment {
            out.push_str(part);
            continue;
        }
        let mut last = 0;
        for (a, b, inner) in placeholders(part) {
            out.push_str(&part[last..a]);
            out.push_str(&format_value(&evaluate(inner, values)?)?);
            last = b;
        }
        out.push_str(&part[last..]);
    }
    Ok(out)
}

fn collect_names(n: &Node, out: &mut BTreeSet<String>) {
    match n {
        Node::Name(s) => {
            out.insert(s.clone());
        }
        Node::Unary(_, a) => collect_names(a, out),
        Node::Binary(_, a, b) => {
            collect_names(a, out);
            collect_names(b, out);
        }
        Node::Compare(a, r) => {
            collect_names(a, out);
            r.iter().for_each(|(_, b)| collect_names(b, out));
        }
        Node::And(v) | Node::Or(v) => v.iter().for_each(|x| collect_names(x, out)),
        Node::Call(name, v) => {
            out.insert(name.clone());
            v.iter().for_each(|x| collect_names(x, out));
        }
        Node::IfElse(a, b, c) => {
            collect_names(a, out);
            collect_names(b, out);
            collect_names(c, out);
        }
        _ => {}
    }
}

/// Parameter-like names used in the text's `{{ }}` expressions (to check a pattern's form).
pub fn names_used(text: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for (comment, part) in split_comments(text) {
        if comment {
            continue;
        }
        for (_, _, inner) in placeholders(part) {
            if let Ok(node) = parse(inner.trim()) {
                collect_names(&node, &mut out);
            }
        }
    }
    out.retain(|n| !FUNCTIONS.contains(&n.as_str()) && !CONSTANTS.contains(&n.as_str()));
    out
}

pub fn has_expressions(text: &str) -> bool {
    !placeholders(text).is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vals(pairs: &[(&str, Value)]) -> Values {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect()
    }

    fn ev(text: &str) -> Value {
        evaluate(text, &Values::new()).unwrap()
    }

    #[test]
    fn arithmetic_only() {
        let v = vals(&[("a", Value::Int(3)), ("b", Value::Int(16))]);
        assert_eq!(evaluate("a * 2 + sqrt(b)", &v).unwrap(), Value::Float(10.0));
        let v = vals(&[
            ("w", Value::Int(2)),
            ("layout", Value::Str("square".into())),
        ]);
        assert_eq!(
            evaluate("w if layout == 'square' else w * sqrt2", &v).unwrap(),
            Value::Int(2)
        );
        let v = vals(&[("a", Value::Int(1))]);
        assert_eq!(
            substitute("<rect width=\"{{ a / 3 }}\"/>", &v).unwrap(),
            "<rect width=\"0.3333\"/>"
        );
        for bad in [
            "__import__('os')",
            "a.real",
            "open('x')",
            "[1, 2]",
            "lambda: 1",
            "2 ** 1000",
        ] {
            assert!(evaluate(bad, &v).is_err(), "{bad} should be refused");
        }
        let e = evaluate("nope + 1", &Values::new()).unwrap_err();
        assert!(e.0.contains("unknown name"), "{}", e.0);
        let names = names_used("x=\"{{ a + b * pi }}\" y=\"{{ sqrt(c) }}\"");
        assert_eq!(
            names,
            ["a", "b", "c"].into_iter().map(String::from).collect()
        );
    }

    #[test]
    fn follows_python_numbers() {
        assert_eq!(ev("7 // 2"), Value::Int(3));
        assert_eq!(ev("-7 // 2"), Value::Int(-4));
        assert_eq!(ev("-7 % 3"), Value::Int(2));
        assert_eq!(ev("7.5 % -2"), Value::Float(-0.5));
        assert_eq!(format_value(&ev("-10 % 1.25")).unwrap(), "0");
        assert_eq!(ev("10 // 0.4"), Value::Float(24.0));
        assert_eq!(ev("-7.5 // 2"), Value::Float(-4.0));
        assert_eq!(ev("2 ** 10"), Value::Int(1024));
        assert_eq!(ev("2 ** -1"), Value::Float(0.5));
        assert_eq!(ev("-2 ** 2"), Value::Int(-4));
        assert_eq!(ev("2 ** 3 ** 2"), Value::Int(512));
        assert_eq!(ev("1 / 4"), Value::Float(0.25));
        assert_eq!(ev("round(2.5)"), Value::Int(2));
        assert_eq!(ev("round(3.5)"), Value::Int(4));
        assert_eq!(ev("round(-0.5)"), Value::Int(0));
        assert_eq!(ev("round(1.23456, 2)"), Value::Float(1.23));
        assert_eq!(ev("int(-2.7)"), Value::Int(-2));
        assert_eq!(ev("floor(-2.5)"), Value::Int(-3));
        assert_eq!(ev("max(1, 2.5, 2)"), Value::Float(2.5));
        assert_eq!(ev("min(3, 1)"), Value::Int(1));
        assert_eq!(ev("1 < 2 < 3"), Value::Bool(true));
        assert_eq!(ev("1 < 3 < 2"), Value::Bool(false));
        assert_eq!(ev("not 0"), Value::Bool(true));
        assert_eq!(ev("1 and 0"), Value::Bool(false));
        assert_eq!(ev("true + 1"), Value::Int(2));
        assert_eq!(ev("abs(-3)"), Value::Int(3));
        assert!((ev("cos(60)").as_f64().unwrap() - 0.5).abs() < 1e-12);
        assert_eq!(ev("'a' == 1"), Value::Bool(false));
        assert!(evaluate("'a' < 1", &Values::new()).is_err());
        assert!(evaluate("1 / 0", &Values::new()).is_err());
        assert!(evaluate("sqrt(-1)", &Values::new()).is_err());
        assert!(evaluate("True", &Values::new()).is_err());
        assert!(evaluate("max(1, key=2)", &Values::new()).is_err());
        assert!(evaluate("(1, 2)", &Values::new()).is_err());
        assert!(evaluate("1 +", &Values::new()).is_err());
        assert!(evaluate("", &Values::new()).is_err());
        assert!(evaluate(&"1+".repeat(300), &Values::new())
            .unwrap_err()
            .0
            .contains("too long"));
    }

    #[test]
    fn formats_like_python() {
        let f = |v: Value| format_value(&v).unwrap();
        assert_eq!(f(Value::Int(3)), "3");
        assert_eq!(f(Value::Float(2.5)), "2.5");
        assert_eq!(f(Value::Float(1.0 / 3.0)), "0.3333");
        assert_eq!(f(Value::Float(-0.00001)), "-0");
        assert_eq!(f(Value::Float(0.00005)), "0.0001");
        assert_eq!(f(Value::Float(1e-9)), "0");
        assert_eq!(f(Value::Bool(true)), "1");
        assert!(format_value(&Value::Float(f64::INFINITY)).is_err());
    }

    #[test]
    fn leaves_comments_alone() {
        let v = vals(&[("a", Value::Int(2))]);
        let text = "<!-- {{ not evaluated }} -->\n<rect x=\"{{ a * 2 }}\"/>";
        assert_eq!(
            substitute(text, &v).unwrap(),
            "<!-- {{ not evaluated }} -->\n<rect x=\"4\"/>"
        );
        assert_eq!(
            names_used(text),
            ["a"].into_iter().map(String::from).collect()
        );
        assert!(has_expressions(text));
        assert!(!has_expressions("<rect/>"));
    }
}
