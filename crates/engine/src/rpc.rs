//! JSON-RPC 2.0 over stdio: how the desktop app drives an engine Session.
//! A port of rpc.py, with the same methods and messages (protocol 2).
//!
//! One JSON message per line on stdin and stdout; logs go to stderr.
//! Requests run one at a time on a worker thread, so `cancel` (and `ping`) are
//! answered while a long call is running. Long calls send notifications while
//! running: `{"jsonrpc": "2.0", "method": "progress", "params": {"id",
//! "fraction", "message"}}`.
//!
//! Error codes: -32700 parse error, -32600 invalid request, -32601 unknown
//! method, -32602 bad params, -32000 engine error (message is for the user),
//! -32001 cancelled, -32603 internal error.
//!
//! Pro adds methods (and takes over some) through a [`Plugin`]; the core
//! never knows what it is (ADR 0004).

use crate::session::{self, Error, Progress, Session};
use serde_json::{json, Map, Value};
use std::io::{BufRead, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::Instant;

pub const PROTOCOL: u32 = 2;
pub const PARSE_ERROR: i64 = -32700;
pub const INVALID_REQUEST: i64 = -32600;
pub const NO_METHOD: i64 = -32601;
pub const BAD_PARAMS: i64 = -32602;
pub const ENGINE_ERROR: i64 = -32000;
pub const CANCELLED: i64 = -32001;
pub const INTERNAL: i64 = -32603;

/// Why a method failed.
#[derive(Debug, Clone, PartialEq)]
pub enum CallError {
    Params(String),
    Session(Error),
}

impl From<Error> for CallError {
    fn from(e: Error) -> Self {
        CallError::Session(e)
    }
}

pub type CallResult = Result<Value, CallError>;

/// Methods added on top of the core's (the Pro edition). Called first: `None`
/// leaves the method to the core.
pub trait Plugin: Send {
    fn call(
        &self,
        s: &mut Session,
        method: &str,
        p: &Map<String, Value>,
        progress: Progress,
    ) -> Option<CallResult>;
}

// ------------------------------------------------------------------ params

fn req<'a>(p: &'a Map<String, Value>, key: &str) -> Result<&'a Value, CallError> {
    p.get(key)
        .ok_or_else(|| CallError::Params(format!("missing '{key}'")))
}

pub fn p_str<'a>(p: &'a Map<String, Value>, key: &str) -> Result<&'a str, CallError> {
    req(p, key)?
        .as_str()
        .ok_or_else(|| CallError::Params(format!("'{key}' must be a string")))
}

pub fn p_int(p: &Map<String, Value>, key: &str) -> Result<i64, CallError> {
    let v = req(p, key)?;
    match v {
        Value::Number(n) if n.is_i64() || n.is_u64() => Ok(n.as_i64().unwrap_or(i64::MAX)),
        _ => Err(CallError::Params(format!("'{key}' must be a whole number"))),
    }
}

pub fn p_num(p: &Map<String, Value>, key: &str) -> Result<f64, CallError> {
    req(p, key)?
        .as_f64()
        .ok_or_else(|| CallError::Params(format!("'{key}' must be a number")))
}

fn p_opt_int(p: &Map<String, Value>, key: &str) -> Result<Option<i64>, CallError> {
    match p.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(_) => p_int(p, key).map(Some),
    }
}

pub fn p_list_of_str(p: &Map<String, Value>, key: &str) -> Result<Vec<String>, CallError> {
    let bad = || CallError::Params(format!("'{key}' must be a list of strings"));
    req(p, key)?
        .as_array()
        .ok_or_else(bad)?
        .iter()
        .map(|v| v.as_str().map(String::from).ok_or_else(bad))
        .collect()
}

/// The target a call names (`"target"`), G-code when it names none.
fn target_of(p: &Map<String, Value>) -> Result<&str, CallError> {
    match p.get("target") {
        None | Some(Value::Null) => Ok(crate::project::GCODE),
        Some(_) => p_str(p, "target"),
    }
}

fn opt<'a>(p: &'a Map<String, Value>, key: &str) -> Option<&'a Value> {
    p.get(key).filter(|v| !v.is_null())
}

fn u32_of(v: i64, key: &str) -> Result<u32, CallError> {
    u32::try_from(v).map_err(|_| CallError::Params(format!("'{key}' must be a whole number")))
}

fn usize_of(v: i64, key: &str) -> Result<usize, CallError> {
    usize::try_from(v).map_err(|_| CallError::Params(format!("'{key}' must be a whole number")))
}

/// Run one core method.
pub fn call(
    s: &mut Session,
    method: &str,
    p: &Map<String, Value>,
    progress: Progress,
) -> Option<CallResult> {
    let r = (|| -> CallResult {
        Ok(match method {
            "initialize" => {
                if let Some(dir) = opt(p, "libraryDir")
                    .and_then(Value::as_str)
                    .filter(|d| !d.is_empty())
                {
                    s.set_library(Path::new(dir));
                }
                json!({"version": crate::VERSION, "protocol": PROTOCOL, "engine": "rust"})
            }
            "open" => s.open(&p_list_of_str(p, "paths")?, progress)?,
            "describe" => s.describe(),
            "replaceInputs" => s.replace_inputs(&p_list_of_str(p, "paths")?, progress)?,
            "toolpaths" => {
                let from = p_opt_int(p, "layerFrom")?
                    .map(|v| u32_of(v, "layerFrom"))
                    .transpose()?;
                let to = p_opt_int(p, "layerTo")?
                    .map(|v| u32_of(v, "layerTo"))
                    .transpose()?;
                s.toolpaths(
                    usize_of(p_int(p, "input")?, "input")?,
                    u32_of(p_int(p, "plate")?, "plate")?,
                    from,
                    to,
                )?
            }
            "setProject" => {
                let base = opt(p, "base").and_then(Value::as_str).map(Path::new);
                s.set_project(req(p, "project")?, base)?
            }
            "loadProject" => s.load_project(Path::new(p_str(p, "path")?), progress)?,
            "saveProject" => s.save_project(Path::new(p_str(p, "path")?))?,
            "assemblyStatus" => s.assembly_status(),
            "defaultPlacements" => s.default_placements(),
            "exportLayout" => s.export_layout(Path::new(p_str(p, "path")?))?,
            "patterns" => s.patterns()?,
            "patternInfo" => s.pattern_info(req(p, "pattern")?)?,
            "patternGeometry" => s.pattern_geometry(req(p, "pattern")?, opt(p, "directions"))?,
            "patternSource" => s.pattern_source(req(p, "pattern")?)?,
            "patternStarter" => s.pattern_starter(),
            "patternPreview" => s.pattern_preview(
                p_str(p, "source")?,
                req(p, "form")?,
                opt(p, "values"),
                opt(p, "directions"),
            )?,
            "savePattern" => s.save_pattern(
                p_str(p, "name")?,
                p_str(p, "source")?,
                req(p, "form")?,
                opt(p, "groups"),
                opt(p, "id").and_then(Value::as_str),
            )?,
            "deletePattern" => s.delete_pattern(p_str(p, "id")?)?,
            "packs" => s.packs(),
            "importPack" => s.import_pack(Path::new(p_str(p, "path")?))?,
            "removePack" => s.remove_pack(p_str(p, "id")?)?,
            "exportPack" => s.export_pack(
                Path::new(p_str(p, "path")?),
                &p_list_of_str(p, "patterns")?,
                req(p, "pack")?,
            )?,
            "importPattern" => s.import_pattern(Path::new(p_str(p, "path")?))?,
            "importImage" => s.import_image(Path::new(p_str(p, "path")?))?,
            "exportPattern" => s.export_pattern(
                req(p, "pattern")?,
                opt(p, "directions"),
                Path::new(p_str(p, "path")?),
                opt(p, "title").and_then(Value::as_str),
            )?,
            "savePreset" => s.save_preset(
                p_str(p, "patternId")?,
                p_str(p, "name")?,
                req(p, "values")?,
                opt(p, "directions"),
                opt(p, "id").and_then(Value::as_str),
            )?,
            "renamePreset" => {
                s.rename_preset(p_str(p, "patternId")?, p_str(p, "id")?, p_str(p, "name")?)?
            }
            "deletePreset" => s.delete_preset(p_str(p, "patternId")?, p_str(p, "id")?)?,
            "targets" => s.targets(),
            "generate" => s.generate_with(target_of(p)?, progress)?,
            "checkResult" => s.check_with(target_of(p)?)?,
            "thumbnail" => {
                let plate = match p.get("plate") {
                    None | Some(Value::Null) => 1,
                    Some(_) => u32_of(p_int(p, "plate")?, "plate")?,
                };
                crate::source::thumbnail_json(Path::new(p_str(p, "path")?), plate)
            }
            "projectSummary" => session::project_summary(Path::new(p_str(p, "path")?), &s.library)?,
            "export" => s.export_with(target_of(p)?, &p_list_of_str(p, "paths")?, progress)?,
            "assembledToolpaths" => s.assembled_toolpaths()?,
            "assembledSolid" => s.assembled_solid()?,
            // the edition (a core-only build has no licences: the Pro plugin answers instead)
            "licence" | "edition" => json!({
                "edition": s.edition.name(),
                "limits": s.edition.describe(),
                "licence": null,
                "problem": null,
                "trial": {"days": 14, "started": null, "ends": null},
                "fingerprint": null,
            }),
            "importLayout" => {
                return Err(CallError::Session(Error::Engine(
                    "layouts are part of TessaFab Pro".into(),
                )))
            }
            _ => return Err(CallError::Params(String::new())),
        })
    })();
    match r {
        Err(CallError::Params(m)) if m.is_empty() => None,
        r => Some(r),
    }
}

// ------------------------------------------------------------------ server

type Out = Arc<Mutex<Box<dyn Write + Send>>>;

fn send(out: &Out, msg: &Value) {
    if let Ok(mut o) = out.lock() {
        let _ = writeln!(o, "{msg}");
        let _ = o.flush();
    }
}

fn reply(out: &Out, id: &Value, result: Result<Value, (i64, String)>) {
    let msg = match result {
        Ok(r) => json!({"jsonrpc": "2.0", "id": id, "result": r}),
        Err((code, message)) => {
            json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
        }
    };
    send(out, &msg);
}

struct Job {
    id: Value,
    method: String,
    params: Map<String, Value>,
    cancel: Arc<AtomicBool>,
}

/// Serve requests from `input` until it ends or `shutdown`; replies to `out`.
pub fn serve(input: impl BufRead, out: Box<dyn Write + Send>, plugin: Option<Box<dyn Plugin>>) {
    serve_with(input, out, plugin, |_| {})
}

/// [`serve`], with `setup` run on the session first: a server's `main`
/// composes its targets in here (ADR 0008).
pub fn serve_with(
    input: impl BufRead,
    out: Box<dyn Write + Send>,
    plugin: Option<Box<dyn Plugin>>,
    setup: fn(&mut Session),
) {
    let out: Out = Arc::new(Mutex::new(out));
    let current: Arc<Mutex<Option<(Value, Arc<AtomicBool>)>>> = Arc::new(Mutex::new(None));
    let running = Arc::new(AtomicBool::new(true));
    let (tx, rx) = mpsc::channel::<Job>();
    let worker = {
        let (out, current, running) = (out.clone(), current.clone(), running.clone());
        std::thread::spawn(move || {
            let mut s = Session::default();
            setup(&mut s);
            for job in rx {
                *current.lock().unwrap() = Some((job.id.clone(), job.cancel.clone()));
                if job.method == "shutdown" {
                    running.store(false, Ordering::SeqCst);
                    reply(&out, &job.id, Ok(json!({})));
                    *current.lock().unwrap() = None;
                    break;
                }
                run_job(&mut s, &job, &out, plugin.as_deref());
                *current.lock().unwrap() = None;
                if !running.load(Ordering::SeqCst) {
                    break;
                }
            }
        })
    };
    for line in input.lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        let msg: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(_) => {
                reply(&out, &Value::Null, Err((PARSE_ERROR, "not JSON".into())));
                continue;
            }
        };
        let id = msg.get("id").cloned().unwrap_or(Value::Null);
        let method = msg.get("method").and_then(Value::as_str);
        let (Some(method), true) = (method, msg.get("jsonrpc") == Some(&json!("2.0"))) else {
            reply(
                &out,
                &id,
                Err((INVALID_REQUEST, "not a JSON-RPC 2.0 request".into())),
            );
            continue;
        };
        let params = match msg.get("params") {
            None | Some(Value::Null) => Map::new(),
            Some(Value::Object(m)) => m.clone(),
            Some(_) => {
                reply(
                    &out,
                    &id,
                    Err((BAD_PARAMS, "params must be an object".into())),
                );
                continue;
            }
        };
        match method {
            "ping" => reply(&out, &id, Ok(json!({}))),
            "cancel" => {
                let cur = current.lock().unwrap();
                let hit = cur
                    .as_ref()
                    .is_some_and(|(rid, _)| Some(rid) == params.get("id"));
                if hit {
                    cur.as_ref().unwrap().1.store(true, Ordering::SeqCst);
                }
                reply(&out, &id, Ok(json!({"cancelled": hit})));
            }
            _ => {
                let job = Job {
                    id,
                    method: method.to_string(),
                    params,
                    cancel: Arc::new(AtomicBool::new(false)),
                };
                if tx.send(job).is_err() {
                    break;
                }
            }
        }
        if !running.load(Ordering::SeqCst) || worker.is_finished() {
            break;
        }
    }
    drop(tx);
    let _ = worker.join();
}

fn run_job(s: &mut Session, job: &Job, out: &Out, plugin: Option<&dyn Plugin>) {
    let mut last: Option<Instant> = None;
    let id = job.id.clone();
    let mut progress = |fraction: f64, message: &str| -> session::Result<()> {
        if job.cancel.load(Ordering::SeqCst) {
            return Err(Error::Cancelled);
        }
        if last.is_none_or(|t| t.elapsed().as_secs_f64() >= 0.1) || fraction >= 1.0 {
            last = Some(Instant::now());
            send(
                out,
                &json!({"jsonrpc": "2.0", "method": "progress",
                        "params": {"id": id, "fraction": (fraction * 1e4).round() / 1e4, "message": message}}),
            );
        }
        Ok(())
    };
    let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        plugin
            .and_then(|pl| pl.call(s, &job.method, &job.params, &mut progress))
            .or_else(|| call(s, &job.method, &job.params, &mut progress))
    }));
    let answer = match res {
        Err(panic) => {
            let what = panic
                .downcast_ref::<String>()
                .cloned()
                .or_else(|| panic.downcast_ref::<&str>().map(|s| s.to_string()))
                .unwrap_or_default();
            eprintln!("internal error in {}: {what}", job.method);
            Err((INTERNAL, format!("internal error: {what}")))
        }
        Ok(None) => Err((NO_METHOD, format!("unknown method '{}'", job.method))),
        Ok(Some(Ok(v))) => Ok(v),
        Ok(Some(Err(CallError::Params(m)))) => Err((BAD_PARAMS, m)),
        Ok(Some(Err(CallError::Session(Error::Cancelled)))) => Err((CANCELLED, "cancelled".into())),
        Ok(Some(Err(CallError::Session(Error::Engine(m))))) => Err((ENGINE_ERROR, m)),
    };
    if !job.id.is_null() || answer.is_err() {
        reply(out, &job.id, answer);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone, Default)]
    struct Buf(Arc<Mutex<Vec<u8>>>);

    impl Write for Buf {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    fn run(input: &str) -> Vec<Value> {
        let buf = Buf::default();
        serve(
            std::io::Cursor::new(input.to_string()),
            Box::new(buf.clone()),
            None,
        );
        let text = String::from_utf8(buf.0.lock().unwrap().clone()).unwrap();
        text.lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }

    #[test]
    fn answers_requests_and_errors() {
        let out = run(concat!(
            "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"ping\"}\n",
            "nonsense\n",
            "{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"nope\"}\n",
            "{\"jsonrpc\":\"2.0\",\"id\":3,\"method\":\"open\",\"params\":{\"paths\":\"x\"}}\n",
            "{\"jsonrpc\":\"2.0\",\"id\":4,\"method\":\"open\",\"params\":{\"paths\":[\"/no/such.gcode\"]}}\n",
            "{\"jsonrpc\":\"2.0\",\"id\":5,\"method\":\"initialize\"}\n",
            "{\"jsonrpc\":\"2.0\",\"id\":6,\"method\":\"shutdown\"}\n",
        ));
        let by_id = |i: i64| out.iter().find(|m| m["id"] == json!(i)).unwrap().clone();
        assert_eq!(by_id(1)["result"], json!({}));
        assert!(out.iter().any(|m| m["error"]["code"] == json!(PARSE_ERROR)));
        assert_eq!(by_id(2)["error"]["code"], json!(NO_METHOD));
        assert_eq!(
            by_id(3)["error"]["message"],
            json!("'paths' must be a list of strings")
        );
        assert_eq!(
            by_id(4)["error"],
            json!({"code": ENGINE_ERROR, "message": "file not found: /no/such.gcode"})
        );
        assert_eq!(by_id(5)["result"]["protocol"], json!(PROTOCOL));
        assert_eq!(by_id(6)["result"], json!({}));
    }
}
