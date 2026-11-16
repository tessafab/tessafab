//! One plate of a project rewritten end to end: run arguments from the
//! project and the slicer, the placed pattern from the assembly, and the
//! top surfaces the user kept excluded. The per-plate part of session.py's
//! generate.

use crate::args::RunArgs;
use crate::assembly::{self, Link};
use crate::colour::{self, ColourError, Plan};
use crate::engine;
use crate::patterns::Library;
use crate::plate::{bottom_surface_id, surface_id, Plate};
use crate::project;
use serde_json::Value;
use std::collections::BTreeMap;

/// A rewritten plate.
#[derive(Debug, Clone)]
pub struct PlateResult {
    pub lines: Vec<String>,
    pub rewritten: usize,
    pub kept: usize,
    pub log: String,
    /// Tool changes and purge (`Plan::cost`), for multi-colour plates.
    pub colour: Option<Value>,
    /// Layers raised tiles added after the original last one (relief).
    pub added_layers: usize,
}

/// The multi-colour plan for one plate, or None for single-colour projects.
/// targets.gcode.colour.filaments numbers filaments from 1, as the slicer shows them.
pub fn colour_plan<'a>(project: &Value, plate: &'a Plate) -> Result<Option<Plan<'a>>, ColourError> {
    let col = &project::gcode(project)["colour"];
    let mode = col["mode"].as_str().unwrap_or("single");
    let Some(fils) = col["filaments"]
        .as_object()
        .filter(|m| !m.is_empty() && mode != "single")
    else {
        return Ok(None);
    };
    let mapping: BTreeMap<String, usize> = fils
        .iter()
        .map(|(g, n)| (g.clone(), (n.as_i64().unwrap_or(1).max(1) - 1) as usize))
        .collect();
    let mode = if mode == "manual" {
        colour::MANUAL
    } else {
        colour::AMS
    };
    let mut plan = Plan::new(plate.config(), &plate.parsed.lines, &mapping, mode, true)?;
    plan.depth = col["depth"].as_u64().unwrap_or(2).max(1) as usize;
    Ok(plan.uses_colour().then_some(plan))
}

/// The project's key for a plate in assembly.assign: "N", or "INPUT:N" when
/// several files are open.
pub fn plate_key(input: usize, plate: u32, inputs: usize) -> String {
    if inputs > 1 {
        format!("{input}:{plate}")
    } else {
        plate.to_string()
    }
}

/// Run arguments for one plate: the project's settings, the slicer's own
/// retraction, z-hop and travel speed where the project sets none, then the
/// defaults that depend on other options.
pub fn plate_args(
    project: &Value,
    plate: &Plate,
    input: usize,
    inputs: usize,
    lib: &Library,
) -> Result<RunArgs, String> {
    let mut args = RunArgs {
        quiet: true,
        ..Default::default()
    };
    let key = plate_key(input, plate.number, inputs);
    project::apply_to_args(project, &mut args, Some(&key), lib).map_err(|e| e.0)?;
    project::apply_slicer_defaults(project, &mut args, plate.config());
    args.finish()?;
    Ok(args)
}

/// Rewrite one plate of a project whose assembly has placements. `links`
/// come from [`assembly::relink`] over every open plate; `name` names the
/// input in messages. `progress(done, total)` follows the surface groups
/// (false stops early: the caller is cancelling).
pub fn generate_plate(
    project: &Value,
    links: &[Link],
    plate: &Plate,
    input: usize,
    inputs: usize,
    name: &str,
    lib: &Library,
    progress: &mut dyn FnMut(usize, usize) -> bool,
) -> Result<PlateResult, String> {
    let args = plate_args(project, plate, input, inputs, lib)?;
    let mut plan = colour_plan(project, plate).map_err(|e| {
        format!(
            "{name} plate {} can't print colour tiles: {}",
            plate.number, e.0
        )
    })?;
    let mut log = String::new();
    let placements = project["assembly"]["placements"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    if placements.is_empty() {
        // a layout SVG the pieces are slid onto (older projects) comes with the Pro layouts
        return Err("this project has no placed pieces; open it in the app to place them".into());
    }
    let key = (input as i64, plate.number as i64);
    let mut placement =
        assembly::build_placement(project, links, plate.pieces(), key, lib, None, &mut |m| {
            log.push_str(m)
        })
        .map_err(|e| format!("{name} plate {}: {}", plate.number, e.0))?;
    // layers under the top: the same tiles, solid (grout is only on top)
    let mut depth_placement = match &plan {
        Some(p) if p.depth > 1 => Some(
            assembly::build_placement(
                project,
                links,
                plate.pieces(),
                key,
                lib,
                Some(0.0),
                &mut |_| {},
            )
            .map_err(|e| format!("{name} plate {}: {}", plate.number, e.0))?,
        ),
        _ => None,
    };
    let excluded: Vec<&str> = project["source"]["surfaces"]["excluded"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    let (top, bottom) = project::faces(project);
    let select = |g: &[&crate::gcode::Block]| {
        let id = surface_id(input, plate.number, g[0].layer, g[0].start.2);
        top && !excluded.contains(&id.as_str())
    };
    let select_bottom = |g: &[&crate::gcode::Block]| {
        let id = bottom_surface_id(input, plate.number, g[0].layer, g[0].start.2);
        !excluded.contains(&id.as_str())
    };
    // the face the pieces lie on: the same tiles without grout, seen from below
    let mut bottom_placement = if bottom {
        Some(
            assembly::build_placement(
                project,
                links,
                plate.pieces(),
                key,
                lib,
                Some(0.0),
                &mut |_| {},
            )
            .map_err(|e| format!("{name} plate {}: {}", plate.number, e.0))?
            .seen_from_below(),
        )
    } else {
        None
    };
    // pattern depth (relief): the pattern carried into the solid layers under the
    // top, grout and all, and over a patterned bottom (no deeper than them)
    let mut eargs = args.engine_args();
    let grout = project::grout_depth(project);
    if grout > 1 {
        eargs.grout_depth = Some(grout);
        let shells = plate
            .config()
            .f64("top_shell_layers")
            .or_else(|| plate.config().f64("top_solid_layers"));
        if let Some(n) = shells.filter(|&n| (grout as f64) > n) {
            log.push_str(&format!(
                "depth: pattern depth {grout} is deeper than the slicer's {n} top shell layers; the pattern stops there\n"
            ));
        }
    }
    // relief: raised tiles (new layers above the tops)
    let raise = project::raise(project);
    if raise.values().any(|&n| n > 0) {
        let cfg = plate.config();
        let by_object = cfg
            .get("print_sequence")
            .is_some_and(|v| matches!(v, crate::slicer::Value::Str(s) if s.contains("object")))
            || cfg.f64("complete_objects").is_some_and(|v| v > 0.0);
        if by_object {
            return Err(format!(
                "{name} plate {}: raised tiles can't be added to a plate printed one object at a time (the nozzle would hit finished objects); print it all at once",
                plate.number
            ));
        }
        let most = raise.values().copied().max().unwrap_or(0) as f64;
        let top = plate
            .toolpaths()
            .layer_z
            .iter()
            .copied()
            .fold(0.0, f64::max);
        let h = args.layer_height;
        let height = cfg
            .f64("printable_height")
            .or_else(|| cfg.f64("max_print_height"));
        if let Some(max) = height.filter(|&m| top + most * h > m) {
            return Err(format!(
                "{name} plate {}: raised tiles would reach {:.2} mm, above the printer's {max:.0} mm",
                plate.number,
                top + most * h
            ));
        }
        eargs.raise = Some(raise);
    }
    let (lines, rewritten, kept, plog, added_layers) = engine::process_plate_faces(
        &plate.parsed,
        &eargs,
        &mut placement,
        Some(&select),
        progress,
        plan.as_mut().map(|p| engine::Colour {
            plan: p,
            depth: depth_placement.as_mut(),
        }),
        bottom_placement.as_mut().map(|pl| engine::Bottom {
            parsed: plate.bottom(),
            pl,
            select: Some(&select_bottom),
            depth: project::bottom_depth(project),
        }),
    );
    log.push_str(&plog);
    let colour = plan.as_ref().map(Plan::cost);
    Ok(PlateResult {
        lines,
        rewritten,
        kept,
        log,
        colour,
        added_layers,
    })
}
