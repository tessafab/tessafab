"""Regenerate the multi-colour Bambu Studio fixtures (spike S1).

Slices tiny test models with the installed Bambu Studio command-line tool and
its system profiles, so the output has real tool changes that Bambu Studio
expanded from its own ``change_filament_gcode`` macro.

    python3 fixtures/multicolour/generate.py [--bambu APP] [case ...]

Notes on driving the CLI (Bambu Studio 02.09.01.70):

- ``--load-settings``/``--load-filaments`` do not resolve ``inherits``/``include``
  in system profiles, so this script flattens them first (parent, then
  includes, then the profile's own keys).
- Without ``filament_colour`` in each filament file the slice silently
  collapses to one filament. With colours set, Bambu derives the flush matrix
  from them, as the GUI does.
- ``--load-filament-ids`` assigns one filament per input STL.

Each case writes ``<case>.gcode`` next to this script. Models are ASCII STL
boxes, one per filament, a few layers tall, so every layer needs tool changes.
"""

from __future__ import annotations

import argparse
import json
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

HERE = Path(__file__).resolve().parent
DEFAULT_APP = Path("/Applications/BambuStudio.app")

X1C = ("Bambu Lab X1 Carbon 0.4 nozzle", "0.20mm Standard @BBL X1C")
P1S = ("Bambu Lab P1S 0.4 nozzle", "0.20mm Standard @BBL X1C")
A1 = ("Bambu Lab A1 0.4 nozzle", "0.20mm Standard @BBL A1")

WHITE, BLACK, RED, BLUE = "#FFFFFF", "#000000", "#C12E1F", "#0A2989"

# box = (x0, y0, size_x, size_y, height, filament number starting at 1)
TWO = [(80, 80, 10, 10, 1.0, 1), (100, 80, 10, 10, 1.0, 2)]
# third box is shorter (fewer filaments on the top layers) and sits at
# x 45..60 so the change-filament travel path has to dodge it
THREE = TWO + [(45, 100, 15, 10, 0.6, 3)]

# name -> (printer, process, [(filament material, colour)], process overrides, boxes)
CASES = {
    "x1c_pla2_tower": (*X1C, [("PLA", WHITE), ("PLA", BLACK)],
                       {"enable_prime_tower": "1"}, TWO),
    "x1c_pla2_notower": (*X1C, [("PLA", WHITE), ("PLA", BLACK)],
                         {"enable_prime_tower": "0"}, TWO),
    "x1c_pla3_tower": (*X1C, [("PLA", WHITE), ("PLA", BLACK), ("PLA", RED)],
                       {"enable_prime_tower": "1"}, THREE),
    # PETG: other temperatures and the PETG branch of the macro
    "x1c_pla2_petg_notower": (*X1C, [("PLA", WHITE), ("PLA", BLACK), ("PETG", BLUE)],
                              {"enable_prime_tower": "0", "curr_bed_type": "Textured PEI Plate"}, THREE),
    # small flush volumes: the tower's 100 mm3 minimum, one flush section,
    # and the macro's short-flush branch without tower
    "x1c_pla2_tower_lowflush": (*X1C, [("PLA", WHITE), ("PLA", BLACK)],
                                {"enable_prime_tower": "1", "flush_multiplier": ["0.3"]}, TWO),
    "x1c_pla3_notower_lowflush": (*X1C, [("PLA", WHITE), ("PLA", BLACK), ("PLA", RED)],
                                  {"enable_prime_tower": "0", "flush_multiplier": ["0.3"]}, THREE),
    "p1s_pla2_tower": (*P1S, [("PLA", WHITE), ("PLA", BLACK)],
                       {"enable_prime_tower": "1"}, TWO),
    "a1_pla2_notower": (*A1, [("PLA", WHITE), ("PLA", BLACK)],
                        {"enable_prime_tower": "0"}, TWO),
    # M9 input: a single-colour print with three filaments set up in the slicer
    # (only the first used), prime tower off, as a user would slice for colour tiles
    "x1c_single_3fil": (*X1C, [("PLA", WHITE), ("PLA", BLACK), ("PLA", RED)],
                        {"enable_prime_tower": "0"}, [(60, 60, 40, 40, 2.0, 1)]),
}

MATERIAL = {"PLA": "Bambu PLA Basic", "PETG": "Bambu PETG Basic"}


# --------------------------------------------------------------------------
# profiles

def index_profiles(root: Path) -> dict[str, Path]:
    idx = {}
    for p in root.rglob("*.json"):
        try:
            name = json.loads(p.read_text()).get("name")
        except (ValueError, UnicodeDecodeError):
            continue
        if name:
            idx.setdefault(name, p)
    return idx


def flatten(name: str, idx: dict[str, Path]) -> dict:
    """Resolve a system profile's ``inherits`` chain and ``include`` templates."""
    own = json.loads(idx[name].read_text())
    out: dict = {}
    if own.get("inherits"):
        out.update(flatten(own["inherits"], idx))
    for inc in own.get("include", []):
        out.update({k: v for k, v in flatten(inc, idx).items()
                    if k not in ("name", "instantiation", "type", "from", "setting_id", "inherits")})
    out.update({k: v for k, v in own.items() if k != "include"})
    out.pop("inherits", None)
    return out


def filament_for(material: str, printer: str, idx: dict[str, Path]) -> str:
    """The system filament preset of this material that lists the printer as compatible."""
    prefix = MATERIAL[material] + " @"
    for name in sorted(idx):
        if not name.startswith(prefix):
            continue
        d = json.loads(idx[name].read_text())
        if d.get("instantiation") == "true" and printer in d.get("compatible_printers", []):
            return name
    raise SystemExit(f"no {material} preset for {printer}")


# --------------------------------------------------------------------------
# models

def write_box(path: Path, x0, y0, sx, sy, sz):
    v = [(x0 + dx * sx, y0 + dy * sy, dz * sz) for dz in (0, 1) for dy in (0, 1) for dx in (0, 1)]
    faces = [(0, 2, 3), (0, 3, 1), (4, 5, 7), (4, 7, 6), (0, 1, 5), (0, 5, 4),
             (2, 6, 7), (2, 7, 3), (0, 4, 6), (0, 6, 2), (1, 3, 7), (1, 7, 5)]
    lines = ["solid box"]
    for f in faces:
        lines += [" facet normal 0 0 0", "  outer loop"]
        lines += ["   vertex %g %g %g" % v[i] for i in f]
        lines += ["  endloop", " endfacet"]
    lines.append("endsolid box")
    path.write_text("\n".join(lines) + "\n")


# --------------------------------------------------------------------------

def run_case(name: str, bambu: Path, idx, work: Path) -> Path:
    printer, process_name, filaments, overrides, boxes = CASES[name]
    d = work / name
    d.mkdir()
    machine = flatten(printer, idx)
    process = flatten(process_name, idx)
    process.update(overrides)
    process.setdefault("compatible_printers", []).append(printer)
    (d / "machine.json").write_text(json.dumps(machine, indent=1))
    (d / "process.json").write_text(json.dumps(process, indent=1))
    fil_files = []
    for i, (material, colour) in enumerate(filaments):
        f = flatten(filament_for(material, printer, idx), idx)
        f["filament_colour"] = [colour] * len(f.get("filament_type", ["x"]))
        p = d / f"filament{i + 1}.json"
        p.write_text(json.dumps(f, indent=1))
        fil_files.append(str(p))
    stls, ids = [], []
    for j, (x0, y0, sx, sy, sz, fil) in enumerate(boxes):
        p = d / f"box{j + 1}.stl"
        write_box(p, x0, y0, sx, sy, sz)
        stls.append(str(p))
        ids.append(str(fil))
    out = d / "out"
    cmd = [str(bambu / "Contents/MacOS/BambuStudio"), "--debug", "1",
           "--allow-multicolor-oneplate", "--arrange", "0",
           "--load-settings", f"{d / 'machine.json'};{d / 'process.json'}",
           "--load-filaments", ";".join(fil_files),
           "--load-filament-ids", ",".join(ids),
           "--slice", "0", "--outputdir", str(out), *stls]
    # cwd: the CLI drops a result.json into its working directory
    r = subprocess.run(cmd, capture_output=True, text=True, cwd=d)
    gcode = out / "plate_1.gcode"
    if not gcode.exists():
        sys.stderr.write(r.stdout + r.stderr)
        raise SystemExit(f"{name}: Bambu Studio produced no G-code (exit {r.returncode})")
    dest = HERE / f"{name}.gcode"
    shutil.copy(gcode, dest)
    print(f"{name}: {dest.stat().st_size // 1024} KB")
    return dest


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--bambu", type=Path, default=DEFAULT_APP, help="BambuStudio.app path")
    ap.add_argument("cases", nargs="*", default=list(CASES))
    a = ap.parse_args(argv)
    idx = index_profiles(a.bambu / "Contents/Resources/profiles/BBL")
    with tempfile.TemporaryDirectory() as tmp:
        for c in a.cases:
            run_case(c, a.bambu, idx, Path(tmp))


if __name__ == "__main__":
    main()
