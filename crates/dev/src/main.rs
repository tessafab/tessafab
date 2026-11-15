//! pp-dev rewrite PLATE.gcode PATTERN.json OUT.gcode
//! pp-dev exprs VALUES_JSON < lines   (evaluates expressions: the expression parity check)
//! pp-dev svg FILE.svg   (the drawing as JSON: the SVG parity check)
//! pp-dev tiles REPEAT LAYOUT ROT OX OY SCALE GROUT DEFAULT_ANGLE   (composed tiles as JSON)
//! pp-dev recontain SRC DST   (plate 1 read and written back: checks the containers)
//!
//! Developer tool for the port: rewrites one plate's top surfaces with a
//! placed pattern dumped by parity/dump.py and prints timings as JSON. The
//! parity harness (parity/parity.py) compares the result with the Python engine.
use std::time::Instant;
use tessafab_core::direction::Direction;
use tessafab_core::{engine, fill, gcode};

fn main() {
    let a: Vec<String> = std::env::args().collect();
    if a.len() == 3 && a[1] == "exprs" {
        // one expression per stdin line, values as JSON in argv: prints the value or ERR per line
        use std::io::BufRead;
        let vals: serde_json::Map<String, serde_json::Value> =
            serde_json::from_str(&a[2]).expect("values JSON");
        let vals: tessafab_core::expr::Values = vals
            .iter()
            .filter_map(|(k, v)| tessafab_core::expr::Value::from_json(v).map(|v| (k.clone(), v)))
            .collect();
        for line in std::io::stdin().lock().lines() {
            let line = line.expect("stdin");
            match tessafab_core::expr::evaluate(&line, &vals)
                .and_then(|v| tessafab_core::expr::format_value(&v))
            {
                Ok(s) => println!("{s}"),
                Err(_) => println!("ERR"),
            }
        }
        return;
    }
    if a.len() == 6 && a[1] == "run" {
        // pp-dev run PROJECT.json INPUT PLATE OUT.gcode: the Rust engine alone rewrites one
        // plate of a project (prints timings as `rewrite` does; the log on stderr)
        let p: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&a[2]).expect("read project"))
                .expect("project JSON");
        let t0 = Instant::now();
        let base = project_dir(&a[2]);
        match run_plate(&p, &base, a[3].parse().unwrap(), a[4].parse().unwrap()) {
            Ok(r) => {
                std::fs::write(&a[5], r.lines.concat()).expect("write");
                eprint!("{}", r.log);
                println!(
                    "{}",
                    serde_json::json!({"total_s": t0.elapsed().as_secs_f64(),
                        "rewritten": r.rewritten, "kept": r.kept})
                );
            }
            Err(e) => {
                eprintln!("{e}");
                std::process::exit(1);
            }
        }
        return;
    }
    if a.len() == 5 && a[1] == "placement" {
        // pp-dev placement PROJECT.json INPUT PLATE: the placed pattern the Rust engine
        // builds for one plate, in parity/parity.py's dump format (or {"error": ...})
        let p: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&a[2]).expect("read project"))
                .expect("project JSON");
        let t0 = Instant::now();
        let base = project_dir(&a[2]);
        let out = match own_placement(&p, &base, a[3].parse().unwrap(), a[4].parse().unwrap()) {
            Ok((pl, log)) => placement_json(&pl, &log),
            Err(e) => serde_json::json!({"error": e}),
        };
        let mut out = out;
        out["seconds"] = serde_json::json!(t0.elapsed().as_secs_f64());
        println!("{out}");
        return;
    }
    if a.len() == 3 && a[1] == "svg" {
        // the drawing as JSON: the SVG parity check
        let text = std::fs::read_to_string(&a[2]).expect("read svg");
        match tessafab_core::svg::read_svg(&text, None, 0.15) {
            Err(e) => println!("{}", serde_json::json!({"error": e.0})),
            Ok(d) => {
                let paint = |p: Option<tessafab_core::svg::Paint>| match p {
                    None => serde_json::Value::Null,
                    Some(p) => serde_json::Value::String(tessafab_core::svg::color_key(p)),
                };
                let items: Vec<_> = d
                    .items
                    .iter()
                    .map(|it| {
                        serde_json::json!({"fill": paint(it.fill), "stroke": paint(it.stroke), "stroke_mm": it.stroke_mm,
                            "subpaths": it.subpaths.iter().map(|(p, c)| serde_json::json!([p, c])).collect::<Vec<_>>(),
                            "id": it.id, "tag": it.tag})
                    })
                    .collect();
                println!(
                    "{}",
                    serde_json::json!({"items": items, "size": [d.size.0, d.size.1], "unit_mm": d.unit_mm})
                );
            }
        }
        return;
    }
    if a.len() == 3 && a[1] == "describe" {
        // pp-dev describe FILE: every plate's description as the app gets it
        use tessafab_core::{container, plate::Plate};
        let path = std::path::Path::new(&a[2]);
        let mut out = Vec::new();
        let t0 = std::time::Instant::now();
        for n in container::plates(path).expect("plates") {
            let data = container::read_plate(path, n).expect("read");
            let lines: Vec<String> = container::split_lines(&data)
                .iter()
                .map(|l| String::from_utf8_lossy(l).into_owned())
                .collect();
            let cfg = (container::kind(path) == container::Kind::Bgcode)
                .then(|| container::config_lines(path, n).expect("config"));
            let p = Plate::new(n, lines, cfg);
            out.push(p.describe(0));
        }
        println!(
            "{}",
            serde_json::json!({"plates": out, "seconds": t0.elapsed().as_secs_f64()})
        );
        return;
    }
    if a.len() == 5 && a[1] == "toolpaths" {
        // pp-dev toolpaths FILE PLATE OUT.bin: the plate's toolpath buffer; prints the
        // solid slabs as JSON ([area, z_bottom, z_top, polygons])
        use tessafab_core::{container, toolpaths};
        let data = container::read_plate(std::path::Path::new(&a[2]), a[3].parse().unwrap())
            .expect("read");
        let lines: Vec<String> = container::split_lines(&data)
            .iter()
            .map(|l| String::from_utf8_lossy(l).into_owned())
            .collect();
        let t0 = std::time::Instant::now();
        let t = toolpaths::extract(&lines, 0.5);
        let t_extract = t0.elapsed().as_secs_f64();
        std::fs::write(&a[4], t.buffer(None, None).0).expect("write");
        let t1 = std::time::Instant::now();
        let slabs = toolpaths::solid_slabs(&t, 0.5, 0.05);
        let t_slabs = t1.elapsed().as_secs_f64();
        let sl: Vec<_> = slabs
            .iter()
            .map(|(g, z0, z1)| serde_json::json!([geo::Area::unsigned_area(g), z0, z1, g.0.len()]))
            .collect();
        println!(
            "{}",
            serde_json::json!({"slabs": sl, "extract_s": t_extract, "slabs_s": t_slabs})
        );
        return;
    }
    if a.len() == 5 && a[1] == "libop" {
        // pp-dev libop LIBRARY_DIR OP ARGS_JSON: a pattern-library operation's result as JSON
        use tessafab_core::patterns::{self, Library};
        let lib = Library::new(Some(std::path::PathBuf::from(&a[2])));
        let args: serde_json::Value = serde_json::from_str(&a[4]).expect("args JSON");
        let r = match a[3].as_str() {
            "list" => lib.list().map(serde_json::Value::from),
            "definition" => lib.definition(args.as_str().unwrap()),
            "check_ref" => patterns::check_ref(&args, &lib),
            "info" => patterns::info(&args, &lib),
            "geometry" => patterns::geometry(Some(&args[0]), Some(&args[1]), &lib),
            op => panic!("unknown op {op}"),
        };
        match r {
            Ok(v) => println!("{v}"),
            Err(e) => println!("{}", serde_json::json!({"error": e.0})),
        }
        return;
    }
    if a.len() == 5 && a[1] == "pattern" {
        // pp-dev pattern ID VALUES_JSON DIRECTIONS_JSON: one repeat's SVG (or ERROR: ...)
        use tessafab_core::library;
        let v: serde_json::Value = serde_json::from_str(&a[3]).expect("values JSON");
        let d: serde_json::Value = serde_json::from_str(&a[4]).expect("directions JSON");
        match library::generate(&a[2], Some(&v), Some(&d), None, None) {
            Ok((text, _)) => print!("{text}"),
            Err(e) => println!("ERROR: {}", e.0),
        }
        return;
    }
    if a.len() == 4 && a[1] == "config" {
        // pp-dev config FILE PLATE: the slicer settings as JSON {key: [kind, value]}
        use tessafab_core::{container, slicer};
        let lines = container::config_lines(std::path::Path::new(&a[2]), a[3].parse().unwrap())
            .expect("read");
        let c = slicer::read_header_config(&lines);
        let mut m = serde_json::Map::new();
        for (k, v) in &c.values {
            m.insert(k.clone(), serde_json::json!([c.kinds[k], format!("{v:?}")]));
        }
        println!("{}", serde_json::Value::Object(m));
        return;
    }
    if a.len() == 4 && a[1] == "others" {
        // pp-dev others PLATE.gcode LAYER: that layer's other extrusions as JSON
        let text = std::fs::read(&a[2]).expect("read");
        let lines = tessafab_core::gcode::read_lines(&String::from_utf8_lossy(&text));
        let parsed = tessafab_core::gcode::find_blocks(lines, 0.42);
        let layer: usize = a[3].parse().unwrap();
        let segs: Vec<[f64; 5]> = parsed
            .others
            .get(&layer)
            .map(|v| v.iter().map(|s| [s.x0, s.y0, s.x1, s.y1, s.w]).collect())
            .unwrap_or_default();
        println!("{}", serde_json::to_string(&segs).unwrap());
        return;
    }
    if a.len() == 3 && a[1] == "drawing" {
        // pp-dev drawing FILE.svg: the tile drawing's summary
        use tessafab_core::{drawing, svg};
        let d =
            svg::read_svg(&std::fs::read_to_string(&a[2]).expect("read"), None, 0.15).expect("svg");
        match drawing::TileDrawing::from_drawing(&d, None, false) {
            Ok(t) => println!("{} | items {}", t.summary(), d.items.len()),
            Err(e) => println!("error: {} | items {}", e.0, d.items.len()),
        }
        return;
    }
    if a.len() == 10 && a[1] == "tiles" {
        // pp-dev tiles REPEAT.svg LAYOUT.svg ROTATE OX OY SCALE GROUT|none DEFAULT_ANGLE|none
        use tessafab_core::{drawing, svg};
        let read = |p: &str| {
            svg::read_svg(&std::fs::read_to_string(p).expect("read svg"), None, 0.15).expect("svg")
        };
        let num = |s: &str| {
            if s == "none" {
                None
            } else {
                Some(s.parse::<f64>().expect("number"))
            }
        };
        let placing = drawing::Placing {
            rotate: a[4].parse().unwrap(),
            origin: (a[5].parse().unwrap(), a[6].parse().unwrap()),
            scale: a[7].parse().unwrap(),
            grout: num(&a[8]),
            grout_scales: false,
            once: false,
        };
        let t0 = std::time::Instant::now();
        let (t, info) =
            drawing::compose_tiles(&read(&a[2]), &read(&a[3]), placing, num(&a[9]), false)
                .expect("compose");
        let secs = t0.elapsed().as_secs_f64();
        let ring = |r: &geo::LineString<f64>| r.0.iter().map(|c| [c.x, c.y]).collect::<Vec<_>>();
        let tiles: Vec<_> = t
            .tiles
            .iter()
            .map(|x| {
                let (angle, concentric) = direction_json(x.direction);
                serde_json::json!({"ext": ring(x.poly.exterior()), "holes": x.poly.interiors().iter().map(ring).collect::<Vec<_>>(),
                    "angle": angle, "concentric": concentric, "color": x.color})
            })
            .collect();
        println!(
            "{}",
            serde_json::json!({"tiles": tiles, "info": info, "summary": t.summary(), "seconds": secs})
        );
        return;
    }
    if a.len() == 4 && a[1] == "recontain" {
        // read plate 1 and write it straight back in the same container (tests the containers)
        let (src, dst) = (std::path::Path::new(&a[2]), std::path::Path::new(&a[3]));
        let text = tessafab_core::container::read_plate(src, 1).expect("read");
        tessafab_core::container::write_plate(src, dst, 1, &text).expect("write");
        return;
    }
    if a.len() != 5 || a[1] != "rewrite" {
        eprintln!("usage: pp-dev rewrite PLATE.gcode PATTERN.json OUT.gcode\n       pp-dev recontain SRC DST");
        std::process::exit(2);
    }
    let a = &a[1..];
    let t0 = Instant::now();
    let bytes = std::fs::read(&a[1]).expect("read plate");
    let text = String::from_utf8_lossy(&bytes);
    let lines = gcode::read_lines(&text);
    let (args, mut placement) =
        engine::load_pattern(&std::fs::read_to_string(&a[2]).expect("read pattern"));
    let t_load = t0.elapsed().as_secs_f64();
    let parsed = gcode::find_blocks(lines, 0.42);
    let t_parse = t0.elapsed().as_secs_f64() - t_load;
    let (out, done, kept, log) = engine::process_plate(&parsed, &args, &mut placement);
    std::fs::write(&a[3], out.concat()).expect("write");
    let total = t0.elapsed().as_secs_f64();
    eprint!("{}", log);
    println!(
        "{{\"load_s\": {:.4}, \"parse_s\": {:.4}, \"total_s\": {:.4}, \"rewritten\": {}, \"kept\": {}}}",
        t_load, t_parse, total, done, kept
    );
}

/// One plate of a project: the Rust engine's own placed pattern and its log.
fn own_placement(
    project: &serde_json::Value,
    base: &std::path::Path,
    input: usize,
    plate: u32,
) -> Result<(fill::PatternedAssembly, String), String> {
    use tessafab_core::{assembly, container, patterns::Library, plate::Plate, project as proj};
    let lib = Library::new(None);
    let p = proj::validate(project, Some(base), &lib).map_err(|e| e.0)?;
    let path = std::path::Path::new(
        p["source"]["inputs"][input]["path"]
            .as_str()
            .ok_or("no such input")?,
    );
    let data = container::read_plate(path, plate).map_err(|e| format!("{e:?}"))?;
    let lines: Vec<String> = container::split_lines(&data)
        .iter()
        .map(|l| String::from_utf8_lossy(l).into_owned())
        .collect();
    let cfg = (container::kind(path) == container::Kind::Bgcode)
        .then(|| container::config_lines(path, plate).expect("config"));
    let pl = Plate::new(plate, lines, cfg);
    let key = (input as i64, plate as i64);
    let pieces = pl.pieces().to_vec();
    let plates = std::collections::BTreeMap::from([(key, pieces.clone())]);
    let placements = p["assembly"]["placements"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let (links, _) = assembly::relink(&placements, &plates);
    let mut log = String::new();
    let placement = assembly::build_placement(&p, &links, &pieces, key, &lib, None, &mut |m| {
        log.push_str(m)
    })
    .map_err(|e| e.0)?;
    Ok((placement, log))
}

/// A placed pattern in parity/parity.py's dump format.
fn placement_json(p: &fill::PatternedAssembly, log: &str) -> serde_json::Value {
    use serde_json::json;
    let ring = |r: &geo::LineString<f64>| r.0.iter().map(|c| [c.x, c.y]).collect::<Vec<_>>();
    let poly = |q: &geo::Polygon<f64>| json!({"ext": ring(q.exterior()), "holes": q.interiors().iter().map(ring).collect::<Vec<_>>()});
    let tiles: Vec<_> = p
        .tiles
        .iter()
        .map(|t| {
            let mut v = poly(&t.poly);
            let (angle, concentric) = direction_json(t.direction);
            v["angle"] = angle;
            v["concentric"] = json!(concentric);
            v["color"] = json!(t.color);
            v
        })
        .collect();
    json!({
        "pieces": p.pieces.iter().map(|m| poly(&m.0[0])).collect::<Vec<_>>(),
        "transforms": p.transforms,
        "tiles": tiles,
        "log": log,
    })
}

/// One plate of a project rewritten by the Rust engine alone.
fn run_plate(
    project: &serde_json::Value,
    base: &std::path::Path,
    input: usize,
    plate: u32,
) -> Result<tessafab_core::generate::PlateResult, String> {
    use tessafab_core::{
        assembly, container, generate, patterns::Library, plate::Plate, project as proj,
    };
    let lib = Library::new(None);
    let p = proj::validate(project, Some(base), &lib).map_err(|e| e.0)?;
    let inputs = p["source"]["inputs"].as_array().map_or(0, Vec::len);
    let path = std::path::Path::new(
        p["source"]["inputs"][input]["path"]
            .as_str()
            .ok_or("no such input")?,
    );
    let data = container::read_plate(path, plate).map_err(|e| format!("{e:?}"))?;
    let lines: Vec<String> = container::split_lines(&data)
        .iter()
        .map(|l| String::from_utf8_lossy(l).into_owned())
        .collect();
    let cfg = (container::kind(path) == container::Kind::Bgcode)
        .then(|| container::config_lines(path, plate).expect("config"));
    let pl = Plate::new(plate, lines, cfg);
    let key = (input as i64, plate as i64);
    let plates = std::collections::BTreeMap::from([(key, pl.pieces().to_vec())]);
    let placements = p["assembly"]["placements"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let (links, _) = assembly::relink(&placements, &plates);
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    generate::generate_plate(&p, &links, &pl, input, inputs, &name, &lib, &mut |_, _| {
        true
    })
}

/// A direction as pp-dev writes it: `angle` (null for rings) and `concentric`.
fn direction_json(d: Direction) -> (serde_json::Value, bool) {
    match d {
        Direction::Angle(a) => (serde_json::json!(a), false),
        Direction::Concentric => (serde_json::Value::Null, true),
    }
}

/// The folder a project file's relative paths are read against, as the app
/// and the slicer hook read them.
fn project_dir(path: &str) -> std::path::PathBuf {
    let abs = std::path::absolute(path).unwrap_or_else(|_| path.into());
    abs.parent()
        .unwrap_or(std::path::Path::new("/"))
        .to_path_buf()
}
