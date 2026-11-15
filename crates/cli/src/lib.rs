//! The command-line tool and slicer hook: rewrite one sliced plate with a
//! saved TessaFab project.
//!
//! ```text
//! tessafab --project FILE [-o OUT] [-q] GCODE
//! ```
//!
//! GCODE is a plain (.gcode) or binary (.bgcode) G-code file of one plate,
//! rewritten in place unless -o is given, so the tool can be a slicer's
//! post-processing script (Bambu Studio, Orca and PrusaSlicer pass the
//! output file as the last argument). The project says which pattern, where
//! the pieces sit in the assembly and the settings; the plate is found among
//! the project's plates by its pieces. On any problem the G-code is left
//! unchanged and the exit status is not 0.
//!
//! The edition sets the limits, as in the app: built from this source, one
//! piece on one plate; TessaFab Pro composes its own edition in
//! ([`main_with`] takes it).

use std::io::Write;
use std::path::PathBuf;
use tessafab_core::args::RunArgs;
use tessafab_core::assembly;
use tessafab_core::container::{self, Kind};
use tessafab_core::edition::{mark_trial, Edition};
use tessafab_core::engine;
use tessafab_core::generate::colour_plan;
use tessafab_core::patterns::Library;
use tessafab_core::plate::Plate;
use tessafab_core::project;

pub const USAGE: &str = "usage: tessafab --project FILE [-o OUT] [-q] GCODE\n\
\n\
Rewrite the top surfaces of a sliced plate (.gcode or .bgcode) with a saved\n\
TessaFab project's pattern. The file is rewritten in place unless -o is\n\
given, so this can be a slicer's post-processing script.\n\
\n\
  --project FILE   the project (.tessafab.json) to apply\n\
  -o, --output OUT write the result here instead\n\
  -q, --quiet      no per-block log\n";

/// Parsed command line.
#[derive(Debug, Default, PartialEq)]
pub struct Cli {
    pub gcode: PathBuf,
    pub project: PathBuf,
    pub output: Option<PathBuf>,
    pub quiet: bool,
}

/// Reads the arguments (without the program name). Err: the message for a usage error.
pub fn parse(argv: &[String]) -> Result<Cli, String> {
    let mut gcode = None;
    let mut project = None;
    let mut output = None;
    let mut quiet = false;
    let mut it = argv.iter();
    while let Some(a) = it.next() {
        let mut value = |name: &str| {
            it.next()
                .map(PathBuf::from)
                .ok_or_else(|| format!("argument {name}: expected one argument"))
        };
        match a.as_str() {
            "--project" => project = Some(value("--project")?),
            "-o" | "--output" => output = Some(value("-o/--output")?),
            "-q" | "--quiet" => quiet = true,
            s if s.starts_with("--project=") => project = Some(PathBuf::from(&s[10..])),
            s if s.starts_with("--output=") => output = Some(PathBuf::from(&s[9..])),
            s if s.starts_with('-') && s.len() > 1 => {
                return Err(format!("unrecognized arguments: {s}"))
            }
            s => {
                if gcode.is_some() {
                    return Err(format!("unrecognized arguments: {s}"));
                }
                gcode = Some(PathBuf::from(s));
            }
        }
    }
    Ok(Cli {
        gcode: gcode.ok_or("the following arguments are required: gcode")?,
        project: project.ok_or(
            "the following arguments are required: --project (the Rust tool works from a saved project)",
        )?,
        output,
        quiet,
    })
}

/// Why a run stopped: the message and the exit status.
#[derive(Debug, PartialEq)]
pub struct Stop(pub String, pub i32);

fn unchanged(what: impl std::fmt::Display) -> Stop {
    Stop(
        format!("tessafab --project: {what}\nThe G-code was left unchanged.\n"),
        1,
    )
}

/// Rewrites the plate; returns the summary line. `log` gets the per-block log.
pub fn run(
    cli: &Cli,
    lib: &Library,
    edition: Edition,
    log: &mut dyn FnMut(&str),
) -> Result<String, Stop> {
    let src = &cli.gcode;
    let read = |e: container::Error| {
        Stop(
            format!("tessafab: can't read {}: {e:?}\n", src.display()),
            1,
        )
    };
    if !src.exists() {
        return Err(Stop(
            format!("tessafab: file not found: {}\n", src.display()),
            1,
        ));
    }
    let project = project::load(&cli.project, lib).map_err(|e| unchanged(e.0))?;
    let kind = container::kind(src);
    if kind == Kind::ThreeMf {
        return Err(Stop(
            "tessafab: give one plate's G-code (.gcode or .bgcode); open a .3mf in the app\n"
                .into(),
            2,
        ));
    }
    let data = container::read_plate(src, 1).map_err(read)?;
    let lines: Vec<String> = container::split_lines(&data)
        .iter()
        .map(|l| String::from_utf8_lossy(l).into_owned())
        .collect();
    let cfg = if kind == Kind::Bgcode {
        Some(container::config_lines(src, 1).map_err(read)?)
    } else {
        None
    };
    let plate = Plate::new(1, lines, cfg);
    edition
        .check_open(1, plate.pieces().len())
        .map_err(unchanged)?;
    if plate.already_processed() {
        return Ok("tessafab: file was already processed, leaving it unchanged\n".into());
    }

    let mut args = RunArgs {
        quiet: cli.quiet,
        ..Default::default()
    };
    project::apply_to_args(&project, &mut args, None, lib).map_err(|e| unchanged(e.0))?;
    project::apply_slicer_defaults(&project, &mut args, plate.config());
    args.finish().map_err(|e| Stop(e, 2))?;
    let mut plan = colour_plan(&project, &plate)
        .map_err(|e| unchanged(format!("can't print colour tiles: {}", e.0)))?;

    let placements = project["assembly"]["placements"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    if placements.is_empty() {
        return Err(unchanged(
            "this project has no placed pieces; open it in the app to place them",
        ));
    }
    let (key, links) =
        assembly::pick_plate(&placements, plate.pieces()).map_err(|e| unchanged(e.0))?;
    edition.check_pattern(links.len()).map_err(unchanged)?;
    let mut text_log = String::new();
    if lib.is_image(&project["pattern"]) {
        edition.check_pro(IMAGE_FEATURE).map_err(unchanged)?;
    }
    let mut placement =
        assembly::build_placement(&project, &links, plate.pieces(), key, lib, None, &mut |m| {
            text_log.push_str(m)
        })
        .map_err(|e| unchanged(e.0))?;
    let mut depth = match &plan {
        Some(p) if p.depth > 1 => Some(
            assembly::build_placement(
                &project,
                &links,
                plate.pieces(),
                key,
                lib,
                Some(0.0),
                &mut |_| {},
            )
            .map_err(|e| unchanged(e.0))?,
        ),
        _ => None,
    };
    // the face the pieces lie on (M20): the same tiles without grout, seen from below
    let (top, bottom) = project::faces(&project);
    let mut below = if bottom {
        let b = assembly::build_placement(
            &project,
            &links,
            plate.pieces(),
            key,
            lib,
            Some(0.0),
            &mut |_| {},
        )
        .map_err(|e| unchanged(e.0))?
        .seen_from_below();
        Some(b)
    } else {
        None
    };
    if !cli.quiet {
        log(&text_log);
    }
    let only_bottom = |_: &[&tessafab_core::gcode::Block]| false;
    let mut eargs = args.engine_args();
    let grout = project::grout_depth(&project);
    if grout > 1 {
        eargs.grout_depth = Some(grout);
    }
    let raise = project::raise(&project);
    if raise.values().any(|&n| n > 0) {
        eargs.raise = Some(raise);
    }
    let (out, done, kept, plog, _) = engine::process_plate_faces(
        &plate.parsed,
        &eargs,
        &mut placement,
        (!top).then_some(&only_bottom as engine::Select),
        &mut |_, _| true,
        plan.as_mut().map(|p| engine::Colour {
            plan: p,
            depth: depth.as_mut(),
        }),
        below.as_mut().map(|pl| engine::Bottom {
            parsed: plate.bottom(),
            pl,
            select: None,
            depth: project::bottom_depth(&project),
        }),
    );
    if !cli.quiet {
        log(&plog);
    }
    let mut out = out;
    if edition == Edition::Trial {
        mark_trial(&mut out);
    }
    let target = cli.output.as_deref().unwrap_or(src);
    container::write_plate(src, target, 1, out.concat().as_bytes()).map_err(|e| {
        Stop(
            format!("tessafab: can't write {}: {e:?}\n", target.display()),
            1,
        )
    })?;
    Ok(format!(
        "tessafab: {done} block(s) rewritten, {kept} kept -> {}\n",
        target.display()
    ))
}

/// The Pro feature an image pattern is (ADR 0015).
const IMAGE_FEATURE: &str = "An image pattern";

/// The whole tool: arguments in, exit status out (messages to stderr).
pub fn main_with(argv: &[String], lib: &Library, edition: Edition) -> i32 {
    let mut err = std::io::stderr();
    if argv.iter().any(|a| a == "-h" || a == "--help") {
        print!("{USAGE}");
        return 0;
    }
    let cli = match parse(argv) {
        Ok(c) => c,
        Err(m) => {
            let _ = writeln!(err, "{USAGE}tessafab: error: {m}");
            return 2;
        }
    };
    let mut log = |s: &str| {
        let _ = std::io::stderr().write_all(s.as_bytes());
    };
    match run(&cli, lib, edition, &mut log) {
        Ok(summary) => {
            if !cli.quiet || summary.contains("already processed") {
                let _ = err.write_all(summary.as_bytes());
            }
            0
        }
        Err(Stop(m, code)) => {
            let _ = err.write_all(m.as_bytes());
            code
        }
    }
}

/// The library the app uses (TESSAFAB_LIBRARY, or the user's folder).
pub fn default_library() -> Library {
    Library::new(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &[&str]) -> Vec<String> {
        s.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn reads_the_slicer_hook_command_line() {
        // the slicer appends the output file to the command
        let c = parse(&v(&["--project", "p.json", "-q", "plate.gcode"])).unwrap();
        assert_eq!(
            c,
            Cli {
                gcode: "plate.gcode".into(),
                project: "p.json".into(),
                output: None,
                quiet: true
            }
        );
        let c = parse(&v(&["in.gcode", "-o", "out.gcode", "--project=p.json"])).unwrap();
        assert_eq!(c.output, Some("out.gcode".into()));
        assert!(parse(&v(&["in.gcode"])).unwrap_err().contains("--project"));
        assert!(parse(&v(&["--project", "p", "a", "b"]))
            .unwrap_err()
            .contains("unrecognized"));
        assert!(parse(&v(&["--project", "p", "--no-such-option", "a"]))
            .unwrap_err()
            .contains("--no-such-option"));
    }
}
