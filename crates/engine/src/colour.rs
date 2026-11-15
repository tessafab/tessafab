//! Multi-colour tiles: the filaments a slice sets up and whether it can take
//! colour tiles. A port of the first part of colour.py; tool changes (the
//! slicer's macro language) come with the multi-colour port.

use crate::slicer::{SlicerConfig, Value};
use serde_json::{json, Value as J};

pub const AMS: &str = "ams";
pub const MANUAL: &str = "manual";

/// `T<n>` on its own (a comment may follow): the tool number.
fn t_line(s: &str) -> Option<u32> {
    // ^T(\d+)\s*(;.*)?$
    let code = s.split_once(';').map(|(c, _)| c).unwrap_or(s).trim_end();
    let num = code.strip_prefix('T')?;
    if num.is_empty() || !num.bytes().all(|c| c.is_ascii_digit()) {
        return None;
    }
    num.parse().ok()
}

/// `M620 S<n>A`: the AMS slot the start G-code loads.
fn m620_line(s: &str) -> Option<u32> {
    let rest = s.strip_prefix("M620 S")?;
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() || !rest[digits.len()..].starts_with('A') {
        return None;
    }
    digits.parse().ok()
}

fn str_at(c: &SlicerConfig, key: &str, i: usize) -> Option<String> {
    match c.get(key)? {
        Value::Strs(v) => v.get(i).or(v.first()).cloned(),
        Value::Str(s) => Some(s.clone()),
        _ => None,
    }
}

/// The filaments set up in the slicer: `[{index, colour, type, temperature}]`.
pub fn filaments(c: &SlicerConfig) -> Vec<J> {
    let cols: Vec<String> = match c.get("filament_colour") {
        Some(Value::Strs(v)) => v.clone(),
        Some(Value::Str(s)) if !s.is_empty() => vec![s.clone()],
        _ => vec![],
    };
    cols.iter()
        .enumerate()
        .map(|(i, col)| {
            let colour = if col.is_empty() { "#808080".to_string() } else { col.clone() };
            let ty = if c.get("filament_type").is_some() { str_at(c, "filament_type", i).unwrap_or_default() } else { String::new() };
            let temp = c.get("nozzle_temperature").and_then(|v| match v {
                Value::Ints(_) | Value::Int(_) => v.at_f64(i).map(|x| json!(x as i64)),
                _ => v.at_f64(i).map(|x| json!(x)),
            });
            json!({"index": i, "colour": colour, "type": ty, "temperature": temp.unwrap_or(J::Null)})
        })
        .collect()
}

/// The filament the print starts with (Bambu: the start G-code's `M620 S<n>A`).
pub fn initial_filament<S: AsRef<str>>(lines: &[S]) -> u32 {
    for line in lines.iter().take(4000) {
        let s = line.as_ref().trim();
        if let Some(n) = m620_line(s).or_else(|| t_line(s)) {
            if n < 255 {
                return n;
            }
        }
    }
    0
}

fn truthy(v: Option<&Value>) -> bool {
    match v {
        None => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Int(i)) => *i != 0,
        Some(Value::Float(f)) => *f != 0.0,
        Some(Value::Str(s)) => !s.is_empty(),
        Some(Value::Strs(v)) => !v.is_empty(),
        Some(_) => true,
    }
}

/// Why this G-code can't take multi-colour tiles (empty if it can).
pub fn input_problems<S: AsRef<str>>(c: &SlicerConfig, lines: &[S], mode: &str) -> Vec<String> {
    let mut problems = Vec::new();
    if truthy(c.get("enable_prime_tower")) {
        problems.push("the slice has a prime tower; slice with the prime tower off".to_string());
    }
    let mut tools = std::collections::BTreeSet::new();
    let mut seen_layer = false;
    for line in lines {
        let s = line.as_ref().trim();
        if s.starts_with(';') && (s.contains("CHANGE_LAYER") || s.contains("LAYER_CHANGE")) {
            seen_layer = true;
        }
        if seen_layer {
            if let Some(n) = t_line(s).filter(|&n| n < 255) {
                tools.insert(n);
            }
        }
    }
    if tools.len() > 1 {
        problems.push("the print already changes filament (it is multi-colour); colour tiles need a single-colour slice".into());
    }
    if c.get("use_relative_e_distances") == Some(&Value::Bool(false)) {
        problems.push("the slice uses absolute E distances; colour tiles need relative E".into());
    }
    if mode == AMS && !truthy(c.get("change_filament_gcode")) {
        problems.push(
            "the slice has no filament change G-code (is this a Bambu printer with an AMS?)".into(),
        );
    }
    problems
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_lines() {
        assert_eq!(t_line("T1"), Some(1));
        assert_eq!(t_line("T12 ; change"), Some(12));
        assert_eq!(t_line("T1 X"), None);
        assert_eq!(t_line("T"), None);
        assert_eq!(m620_line("M620 S3A"), Some(3));
        assert_eq!(m620_line("M620 S255A"), Some(255));
        assert_eq!(m620_line("M620 S3"), None);
    }
}

// --------------------------------------------------------------------------- //
// The plan: which filament each tile group prints in, and the tool changes
// --------------------------------------------------------------------------- //

use crate::slicer_macro::{self as sm, ChangeAt, Opt, Var};
use std::collections::{BTreeMap, HashMap};

/// Bambu Studio's plate types and the filament settings holding their bed temperatures.
const BED_TEMP_KEYS: [(&str, &str); 6] = [
    ("Cool Plate", "cool_plate_temp"),
    ("Engineering Plate", "eng_plate_temp"),
    ("High Temp Plate", "hot_plate_temp"),
    ("Textured PEI Plate", "textured_plate_temp"),
    ("Supertack Plate", "supertack_plate_temp"),
    ("Textured Cool Plate", "textured_cool_plate_temp"),
];

/// A plate can't take colour tiles (the message is for the user).
#[derive(Debug, Clone, PartialEq)]
pub struct ColourError(pub String);

impl std::fmt::Display for ColourError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ColourError {}

fn ints_of(v: Option<&Value>) -> Option<Vec<Option<i64>>> {
    match v? {
        Value::Ints(x) => Some(x.clone()),
        Value::Floats(x) => Some(x.iter().map(|e| e.map(|f| f as i64)).collect()),
        Value::Int(i) => Some(vec![Some(*i)]),
        Value::Float(f) => Some(vec![Some(*f as i64)]),
        _ => None,
    }
}

/// bed_temperature and bed_temperature_initial_layer per filament for the
/// plate type, as Bambu Studio sets them before expanding filament G-code.
fn bed_temperatures(c: &SlicerConfig) -> Option<(Vec<Option<i64>>, Vec<Option<i64>>)> {
    let bed = c.get("curr_bed_type")?.as_str()?;
    let key = BED_TEMP_KEYS.iter().find(|(b, _)| *b == bed)?.1;
    Some((
        ints_of(c.get(key))?,
        ints_of(c.get(&format!("{key}_initial_layer")))?,
    ))
}

fn first_f64(c: &SlicerConfig, key: &str) -> Option<f64> {
    c.get(key).and_then(Value::as_f64)
}

fn at_or(c: &SlicerConfig, key: &str, idx: usize, default: f64) -> f64 {
    c.at_f64(key, idx).unwrap_or(default)
}

/// `M106 ...` arguments: the S value, and whether it is the auxiliary fan (P2)
/// or the part fan (no P, or P0).
fn fan_line(s: &str) -> Option<(String, Option<bool>)> {
    let args = s.strip_prefix("M106")?;
    if args.starts_with(|c: char| c.is_ascii_alphanumeric() || c == '_') {
        return None;
    }
    let words: Vec<&str> = args.split_whitespace().collect();
    let sv = words.iter().find_map(|w| {
        let v = w.strip_prefix('S')?;
        let digits: String = v
            .chars()
            .take_while(|c| c.is_ascii_digit() || *c == '.')
            .collect();
        (!digits.is_empty() && digits.chars().next().unwrap().is_ascii_digit()).then_some(digits)
    })?;
    let p = words.iter().find_map(|w| {
        w.strip_prefix('P')
            .filter(|r| r.starts_with(|c: char| c.is_ascii_digit()))
    });
    let which = match p {
        Some("2") => Some(true),
        None | Some("0") => Some(false),
        _ => None,
    };
    Some((sv, which))
}

/// What colour each tile group prints in, for one plate, and the state of the changes.
pub struct Plan<'a> {
    cfg: &'a SlicerConfig,
    lines: &'a [String],
    pub mode: String,
    fils: Vec<J>,
    /// The filament the print is already using.
    pub base: usize,
    mapping: HashMap<String, usize>,
    bboxes: Option<Vec<[f64; 4]>>,
    /// Layers coloured, counting down from the top.
    pub depth: usize,
    /// Tool changes so far (Bambu's toolchange_count).
    count: i64,
    /// (layer, prev, next) of each change made.
    pub changes: Vec<(usize, usize, usize)>,
    /// Open object label and fan speeds before a line (scanned up to `upto`).
    scan: (usize, Option<String>, Option<String>, Option<String>),
}

impl<'a> Plan<'a> {
    /// `mapping`: tile group -> filament (0-based).
    pub fn new(
        cfg: &'a SlicerConfig,
        lines: &'a [String],
        mapping: &BTreeMap<String, usize>,
        mode: &str,
        check: bool,
    ) -> Result<Self, ColourError> {
        let fils = filaments(cfg);
        let base = initial_filament(lines) as usize;
        let mut map = HashMap::new();
        for (group, &fil) in mapping {
            if fil >= fils.len().max(1) {
                return Err(ColourError(format!(
                    "filament {} isn't set up in the slicer (it has {}); add it in the slicer and slice again",
                    fil + 1,
                    fils.len()
                )));
            }
            map.insert(group.clone(), fil);
        }
        let plan = Plan {
            cfg,
            lines,
            mode: mode.to_string(),
            fils,
            base,
            mapping: map,
            bboxes: None,
            depth: 1,
            count: 0,
            changes: Vec::new(),
            scan: (0, None, None, None),
        };
        if check && plan.uses_colour() {
            let problems = input_problems(cfg, lines, mode);
            if !problems.is_empty() {
                return Err(ColourError(problems.join("; ")));
            }
        }
        Ok(plan)
    }

    pub fn uses_colour(&self) -> bool {
        self.mapping.values().any(|&f| f != self.base)
    }

    pub fn filament_of(&self, group: &str) -> usize {
        self.mapping.get(group).copied().unwrap_or(self.base)
    }

    pub fn checkpoint(&self) -> (i64, usize) {
        (self.count, self.changes.len())
    }

    pub fn rollback(&mut self, cp: (i64, usize)) {
        self.count = cp.0;
        self.changes.truncate(cp.1);
    }

    /// Open object label (its M624 line) and fan speeds (S, P2 S) before line `index`.
    fn state(&mut self, index: usize) -> (Option<String>, Option<String>, Option<String>) {
        if index < self.scan.0 {
            self.scan = (0, None, None, None);
        }
        let (mut label, mut fan, mut aux) =
            (self.scan.1.take(), self.scan.2.take(), self.scan.3.take());
        for line in &self.lines[self.scan.0..index.min(self.lines.len())] {
            let s = line.trim();
            if s.starts_with("M624 ") {
                label = Some(s.to_string());
            } else if s.starts_with("M625") {
                label = None;
            } else if let Some((sv, which)) = fan_line(s) {
                match which {
                    Some(true) => aux = Some(sv),
                    Some(false) => fan = Some(sv),
                    None => {}
                }
            }
        }
        self.scan = (
            index.min(self.lines.len()),
            label.clone(),
            fan.clone(),
            aux.clone(),
        );
        (label, fan, aux)
    }

    fn bboxes(&mut self) -> Vec<[f64; 4]> {
        if self.bboxes.is_none() {
            self.bboxes = Some(sm::object_bboxes_from_gcode(self.lines));
        }
        self.bboxes.clone().unwrap()
    }

    fn expand(&self, key: &str, fil: usize, v: &sm::Vars) -> Result<Vec<String>, ColourError> {
        let tpl = match self.cfg.get(key) {
            Some(Value::Strs(t)) => t.get(fil).or(t.first()).cloned().unwrap_or_default(),
            Some(Value::Str(t)) => t.clone(),
            _ => return Ok(vec![]),
        };
        if tpl.is_empty() {
            return Ok(vec![]);
        }
        let text =
            sm::expand(&tpl, v, Some(self.cfg), fil as i64).map_err(|e| ColourError(e.message))?;
        Ok(text_lines(&text))
    }

    /// G-code lines that switch from filament `prev` to `nxt` and return to
    /// `to_xy` at height z.
    pub fn change(
        &mut self,
        prev: usize,
        nxt: usize,
        to_xy: (f64, f64),
        z: f64,
        layer: usize,
        block_index: usize,
    ) -> Result<Vec<String>, ColourError> {
        self.count += 1;
        self.changes.push((layer, prev, nxt));
        let (label, fan, _aux) = self.state(block_index);
        let cfg = self.cfg;
        let f = sm::fmt_double;
        let travel_f = (first_f64(cfg, "travel_speed").unwrap_or(300.0) * 60.0).round() as i64;
        let zhop = match first_f64(cfg, "z_hop") {
            Some(v) if v != 0.0 => v,
            _ => 0.4,
        };
        let retract = at_or(cfg, "retraction_length", prev, 0.8);
        let retract_f = if cfg.get("retraction_speed").is_some() {
            (at_or(cfg, "retraction_speed", prev, 30.0) * 60.0).round() as i64
        } else {
            1800
        };
        let mut out = vec![format!(
            "; tessafab: filament {} -> {}\n",
            prev + 1,
            nxt + 1
        )];
        out.push(format!("G1 E-{} F{retract_f}\n", f(retract)));
        if label.is_some() {
            out.push("M625\n".into());
        }
        if self.mode == MANUAL {
            out.push(format!("G1 Z{} F{travel_f}\n", f(z + zhop.max(0.4) + 10.0)));
            let pause = cfg
                .get("machine_pause_gcode")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .unwrap_or("M400 U1")
                .to_string();
            let colour = self
                .fils
                .get(nxt)
                .and_then(|x| x["colour"].as_str())
                .unwrap_or("?")
                .to_string();
            out.push(format!(
                "; tessafab: load filament {} ({colour}) and resume\n",
                nxt + 1
            ));
            out.push(pause.trim_end_matches('\n').to_string() + "\n");
        } else {
            let first_layer = layer <= 1;
            let at = ChangeAt {
                layer_z: z,
                max_layer_z: z,
                toolchange_count: self.count,
                first_layer,
                wipe_tower: false,
                layer_num: Some(layer.saturating_sub(1) as i64),
                object_bboxes: Some(self.bboxes()),
                position: Some((to_xy.0, to_xy.1, z)),
            };
            let mut v = sm::toolchange_variables(cfg, prev as i64, nxt, &at);
            v.insert(
                "current_extruder".into(),
                Var::Set(Opt::new("Int", Value::Int(prev as i64))),
            );
            if let Some((bed, bed0)) = bed_temperatures(cfg) {
                v.insert(
                    "bed_temperature".into(),
                    Var::Set(Opt::new("Ints", Value::Ints(bed))),
                );
                v.insert(
                    "bed_temperature_initial_layer".into(),
                    Var::Set(Opt::new("Ints", Value::Ints(bed0))),
                );
            }
            out.extend(self.expand("filament_end_gcode", prev, &v)?);
            out.push(format!("G1 Z{} F{travel_f}\n", f(z + zhop)));
            let macro_text = cfg
                .get("change_filament_gcode")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let text = sm::expand(&macro_text, &v, Some(cfg), nxt as i64)
                .map_err(|e| ColourError(e.message))?;
            out.extend(text_lines(&text));
            // Bambu then sets the part fan for the new filament on this layer, and the
            // auxiliary fan to the new filament's setting while the part fan runs.
            let fan_s = self.fan_after(prev, nxt, layer as i64 - 1, fan.as_deref());
            out.push(format!("M106 S{fan_s}\n"));
            if truthy(cfg.get("auxiliary_fan")) {
                let pct = at_or(cfg, "additional_cooling_fan_speed", nxt, 0.0);
                let on = fan_s.parse::<f64>().unwrap_or(0.0) > 0.0;
                out.push(format!(
                    "M106 P2 S{}\n",
                    if on { (pct * 255.0 / 100.0) as i64 } else { 0 }
                ));
            }
            let key = if first_layer {
                "nozzle_temperature_initial_layer"
            } else {
                "nozzle_temperature"
            };
            out.push(format!(
                "M104 S{} ; set nozzle temperature\n",
                at_or(cfg, key, nxt, 0.0) as i64
            ));
            v.insert(
                "current_extruder".into(),
                Var::Set(Opt::new("Int", Value::Int(nxt as i64))),
            );
            out.extend(self.expand("filament_start_gcode", nxt, &v)?);
        }
        if let Some(l) = label {
            out.push(l + "\n");
        }
        out.push(format!("G1 X{} Y{} F{travel_f}\n", f(to_xy.0), f(to_xy.1)));
        out.push(format!("G1 Z{}\n", f(z)));
        if self.mode == MANUAL {
            out.push(format!("G1 E{} F{retract_f}\n", f(retract)));
        } else {
            let back = at_or(cfg, "retract_length_toolchange", nxt, 2.0);
            let dsp = match first_f64(cfg, "deretraction_speed") {
                Some(v) if v != 0.0 => v,
                _ => first_f64(cfg, "retraction_speed").unwrap_or(30.0),
            };
            out.push(format!(
                "G1 E{} F{}\n",
                f(back),
                (dsp * 60.0).round() as i64
            ));
        }
        Ok(out)
    }

    /// Part fan speed (M106 S value) for filament `nxt` after a change, as
    /// Bambu writes it: off for the filament's first close_fan_the_first_x_layers
    /// layers; between filaments of the same material the speed already
    /// running; otherwise the new filament's fan_max_speed.
    pub fn fan_after(
        &self,
        prev: usize,
        nxt: usize,
        layer_index: i64,
        fan_before: Option<&str>,
    ) -> String {
        let cfg = self.cfg;
        let close = at_or(cfg, "close_fan_the_first_x_layers", nxt, 1.0) as i64;
        if layer_index < close {
            return "0".into();
        }
        let types: Vec<String> = match cfg.get("filament_type") {
            Some(Value::Strs(t)) => t.clone(),
            _ => vec![],
        };
        let same = prev < types.len() && nxt < types.len() && types[prev] == types[nxt];
        if let (true, Some(f)) = (same, fan_before) {
            return f.to_string();
        }
        let mx = at_or(cfg, "fan_max_speed", nxt, 100.0);
        let v: f64 = format!("{:.2}", mx * 255.0 / 100.0).parse().unwrap_or(0.0);
        crate::svg::fmt_g(v)
    }

    /// Tool changes made and the filament they purge: {changes, purge_mm3, purge_g, by_layer}.
    pub fn cost(&self) -> J {
        let (mut purge, mut grams) = (0.0, 0.0);
        for &(_, prev, nxt) in &self.changes {
            if self.mode == MANUAL {
                continue;
            }
            let vol = sm::flush_volume(self.cfg, prev, nxt, false);
            purge += vol;
            grams += vol / 1000.0 * at_or(self.cfg, "filament_density", nxt, 1.24);
        }
        let mut layers = serde_json::Map::new();
        for &(layer, _, _) in &self.changes {
            let k = layer.to_string();
            let n = layers.get(&k).and_then(J::as_i64).unwrap_or(0);
            layers.insert(k, json!(n + 1));
        }
        let r = |x: f64, d: i32| (x * 10f64.powi(d)).round() / 10f64.powi(d);
        json!({"changes": self.changes.len(), "purge_mm3": r(purge, 1), "purge_g": r(grams, 2), "by_layer": layers})
    }
}

fn text_lines(text: &str) -> Vec<String> {
    text.split('\n')
        .filter(|l| !l.trim().is_empty())
        .map(|l| format!("{l}\n"))
        .collect()
}

// --------------------------------------------------------------------------- //
// Filament use and the .gcode.3mf metadata that lists it
// --------------------------------------------------------------------------- //

/// mm of filament extruded per filament index (relative E), following T<n> changes.
pub fn filament_use<S: AsRef<str>>(lines: &[S], start: usize) -> BTreeMap<usize, f64> {
    let mut use_mm = BTreeMap::new();
    let mut cur = start;
    let mut rel = true;
    for line in lines {
        let s = line.as_ref().split(';').next().unwrap_or("").trim();
        if s.is_empty() {
            continue;
        }
        if s == "M82" {
            rel = false;
        } else if s == "M83" {
            rel = true;
        }
        if let Some(n) = t_line(s).filter(|&n| n < 255) {
            cur = n as usize;
            continue;
        }
        if rel
            && ["G1 ", "G0 ", "G2 ", "G3 "]
                .iter()
                .any(|p| s.starts_with(p))
        {
            if let Some(v) = e_word(s).filter(|&v| v > 0.0) {
                *use_mm.entry(cur).or_insert(0.0) += v;
            }
        }
    }
    use_mm
}

/// `\bE(-?\d*\.?\d+)`: the E value of a move.
fn e_word(s: &str) -> Option<f64> {
    let b = s.as_bytes();
    for i in 0..b.len() {
        if b[i] == b'E' && (i == 0 || !(b[i - 1].is_ascii_alphanumeric() || b[i - 1] == b'_')) {
            let num: String = s[i + 1..]
                .chars()
                .enumerate()
                .take_while(|(k, c)| c.is_ascii_digit() || *c == '.' || (*k == 0 && *c == '-'))
                .map(|(_, c)| c)
                .collect();
            if let Ok(v) = num.parse() {
                return Some(v);
            }
        }
    }
    None
}

fn set_attr(tag: &str, key: &str, value: &str) -> String {
    let pat = format!(" {key}=\"");
    if let Some(i) = tag.find(&pat) {
        let start = i + pat.len();
        let end = start + tag[start..].find('"').unwrap_or(0);
        format!("{}{}{}", &tag[..start], xml_escape(value), &tag[end..])
    } else {
        let close = if tag.trim_end().ends_with("/>") {
            tag.rfind("/>").unwrap()
        } else {
            tag.rfind('>').unwrap()
        };
        let head = tag[..close].trim_end();
        format!(
            "{head} {key}=\"{}\"{}{}",
            xml_escape(value),
            if tag[..close].ends_with(' ') { " " } else { "" },
            &tag[close..]
        )
    }
}

fn get_attr<'t>(tag: &'t str, key: &str) -> Option<&'t str> {
    let pat = format!(" {key}=\"");
    let i = tag.find(&pat)? + pat.len();
    let j = tag[i..].find('"')?;
    Some(&tag[i..i + j])
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('"', "&quot;")
}

fn cfg_str_at(c: &SlicerConfig, key: &str, i: usize) -> Option<String> {
    match c.get(key)? {
        Value::Strs(v) => v.get(i).or(v.first()).cloned(),
        Value::Str(s) => Some(s.clone()),
        v => v.at_f64(i).map(crate::svg::fmt_g),
    }
}

/// slice_info.config with a `<filament>` entry for every filament the plate
/// now uses (edited in place: the rest of the file keeps its formatting).
pub fn update_slice_info(
    xml: &str,
    plate: u32,
    use_mm: &BTreeMap<usize, f64>,
    c: &SlicerConfig,
) -> String {
    let mut out = String::with_capacity(xml.len() + 512);
    let mut rest = xml;
    while let Some(i) = rest.find("<plate>") {
        let Some(j) = rest[i..].find("</plate>").map(|j| i + j) else {
            break;
        };
        out.push_str(&rest[..i]);
        let body = &rest[i..j];
        out.push_str(&edit_plate(body, plate, use_mm, c));
        rest = &rest[j..];
    }
    out.push_str(rest);
    out
}

// The weights use 3.14159265, as the Python engine writes them.
#[allow(clippy::approx_constant)]
fn edit_plate(body: &str, plate: u32, use_mm: &BTreeMap<usize, f64>, c: &SlicerConfig) -> String {
    let index_tag = format!("<metadata key=\"index\" value=\"{plate}\"");
    if !body.contains(&index_tag) {
        return body.to_string();
    }
    let lines: Vec<&str> = body.split_inclusive('\n').collect();
    let fil_lines: Vec<usize> = (0..lines.len())
        .filter(|&k| lines[k].trim_start().starts_with("<filament "))
        .collect();
    let Some(&anchor) = fil_lines.first() else {
        return body.to_string();
    };
    let template = lines[anchor].to_string();
    let existing: HashMap<String, usize> = fil_lines
        .iter()
        .filter_map(|&k| get_attr(lines[k], "id").map(|id| (id.to_string(), k)))
        .collect();
    let mut new_fils: Vec<(usize, String)> = Vec::new(); // (line to insert before, text)
    let mut replaced: HashMap<usize, String> = HashMap::new();
    for (k, (&f, &mm)) in use_mm.iter().enumerate() {
        let fid = (f + 1).to_string();
        let (slot, mut tag) = match existing.get(&fid) {
            Some(&line) => (Some(line), lines[line].to_string()),
            None => (None, template.clone()),
        };
        tag = set_attr(&tag, "id", &fid);
        if let Some(t) = cfg_str_at(c, "filament_type", f) {
            tag = set_attr(&tag, "type", &t);
        }
        if let Some(col) = cfg_str_at(c, "filament_colour", f) {
            tag = set_attr(&tag, "color", &col);
        }
        tag = set_attr(&tag, "used_m", &format!("{:.2}", mm / 1000.0));
        let d = at_or(c, "filament_diameter", f, 1.75);
        let dens = at_or(c, "filament_density", f, 1.24);
        tag = set_attr(
            &tag,
            "used_g",
            &format!("{:.2}", mm * 3.14159265 * d * d / 4.0 / 1000.0 * dens),
        );
        match slot {
            Some(line) => {
                replaced.insert(line, tag);
            }
            None => new_fils.push((anchor + k, tag)),
        }
    }
    let used: Vec<String> = use_mm.keys().map(|f| f.to_string()).collect();
    let mut out = String::new();
    for (k, line) in lines.iter().enumerate() {
        for (_, t) in new_fils.iter().filter(|(at, _)| *at == k) {
            out.push_str(t);
        }
        let mut l = replaced
            .get(&k)
            .cloned()
            .unwrap_or_else(|| line.to_string());
        if l.trim_start().starts_with("<layer_filament_list ") {
            l = set_attr(&l, "filament_list", &used.join(" "));
        } else if l.contains("<metadata key=\"filament_maps\"") {
            l = set_attr(&l, "value", &vec!["1"; used.len()].join(" "));
        }
        out.push_str(&l);
    }
    for (_, t) in new_fils.iter().filter(|(at, _)| *at >= lines.len()) {
        out.push_str(t);
    }
    out
}

/// Metadata/plate_N.json with the filaments the plate now uses.
pub fn update_plate_json(
    data: &str,
    use_mm: &BTreeMap<usize, f64>,
    c: &SlicerConfig,
) -> Option<String> {
    let mut d: J = serde_json::from_str(data).ok()?;
    let used: Vec<usize> = use_mm.keys().copied().collect();
    d["filament_ids"] = json!(used);
    if c.get("filament_colour").is_some() {
        d["filament_colors"] = json!(used
            .iter()
            .map(|&f| cfg_str_at(c, "filament_colour", f))
            .collect::<Vec<_>>());
    }
    Some(d.to_string())
}

/// Metadata/filament_sequence.json with the plate's filament order.
pub fn update_filament_sequence(data: &str, plate: u32, use_mm: &BTreeMap<usize, f64>) -> String {
    let mut d: J = serde_json::from_str(data).unwrap_or_else(|_| json!({}));
    if !d.is_object() {
        d = json!({});
    }
    let key = format!("plate_{plate}");
    let mut entry = d
        .get(&key)
        .cloned()
        .filter(J::is_object)
        .unwrap_or_else(|| json!({}));
    let used: Vec<usize> = use_mm.keys().copied().collect();
    entry["sequence"] = json!(used.iter().map(|f| f + 1).collect::<Vec<_>>());
    if entry.get("optimal_assignment").is_some() {
        entry["optimal_assignment"] = json!(vec![0; used.len()]);
    }
    d[key] = entry;
    d.to_string()
}

#[cfg(test)]
mod plan_tests {
    use super::*;

    const INFO: &str = r##"<?xml version="1.0" encoding="UTF-8"?>
<config>
  <plate>
    <metadata key="index" value="1"/>
    <metadata key="filament_maps" value="1"/>
    <filament id="1" tray_info_idx="" type="PLA" color="#00AE42" used_m="5.52" used_g="0.00" group_id="0"/>
    <layer_filament_lists>
      <layer_filament_list filament_list="0" layer_ranges="0 14" />
    </layer_filament_lists>
  </plate>
</config>
"##;

    #[test]
    fn slice_info_lists_every_filament_used() {
        let mut c = SlicerConfig::default();
        c.values.insert(
            "filament_colour".into(),
            Value::Strs(vec!["#00AE42".into(), "#FF0000".into()]),
        );
        c.values.insert(
            "filament_type".into(),
            Value::Strs(vec!["PLA".into(), "PETG".into()]),
        );
        let use_mm = BTreeMap::from([(0, 5000.0), (1, 1200.0)]);
        let out = update_slice_info(INFO, 1, &use_mm, &c);
        assert!(
            out.contains(
                r##"<filament id="1" tray_info_idx="" type="PLA" color="#00AE42" used_m="5.00""##
            ),
            "{out}"
        );
        assert!(
            out.contains(
                r##"<filament id="2" tray_info_idx="" type="PETG" color="#FF0000" used_m="1.20""##
            ),
            "{out}"
        );
        assert!(out.contains(r#"filament_list="0 1""#));
        assert!(out.contains(r#"<metadata key="filament_maps" value="1 1"/>"#));
        assert_eq!(update_slice_info(INFO, 2, &use_mm, &c), INFO);
    }

    #[test]
    fn filament_use_follows_tool_changes() {
        let g = [
            "M83",
            "G1 X1 E2",
            "T1",
            "G1 X2 E0.5",
            "G1 E-0.8",
            "T255",
            "G1 X3 E1",
        ];
        assert_eq!(filament_use(&g, 0), BTreeMap::from([(0, 2.0), (1, 1.5)]));
    }

    #[test]
    fn fan_lines() {
        assert_eq!(fan_line("M106 S255"), Some(("255".into(), Some(false))));
        assert_eq!(fan_line("M106 P2 S100"), Some(("100".into(), Some(true))));
        assert_eq!(fan_line("M106 P3 S100"), Some(("100".into(), None)));
        assert_eq!(fan_line("M1060 S1"), None);
    }
}
