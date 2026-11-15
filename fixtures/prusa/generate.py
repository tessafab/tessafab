"""Regenerate the PrusaSlicer fixtures: two flat tiles sliced for the MINI,
once as binary G-code (.bgcode, the MINI's default) and once as plain G-code.

    python generate.py [path/to/PrusaSlicer]

Needs the "Original Prusa MINI & MINI+" printer profile installed in PrusaSlicer.
"""
import os
import subprocess
import sys
import tempfile

HERE = os.path.dirname(os.path.abspath(__file__))
DEFAULT = "/Applications/Original Prusa Drivers/PrusaSlicer.app/Contents/MacOS/PrusaSlicer"
PROFILES = ["--printer-profile", "Original Prusa MINI & MINI+",
            "--print-profile", "0.20mm QUALITY @MINI",
            "--material-profile", "Prusament PLA"]


def box(x0, y0, w, d, h):
    """ASCII STL triangles of an axis-aligned box."""
    x1, y1 = x0 + w, y0 + d
    v = [(x0, y0, 0), (x1, y0, 0), (x1, y1, 0), (x0, y1, 0), (x0, y0, h), (x1, y0, h), (x1, y1, h), (x0, y1, h)]
    faces = [(0, 2, 1), (0, 3, 2), (4, 5, 6), (4, 6, 7), (0, 1, 5), (0, 5, 4),
             (1, 2, 6), (1, 6, 5), (2, 3, 7), (2, 7, 6), (3, 0, 4), (3, 4, 7)]
    out = []
    for a, b, c in faces:
        out.append("facet normal 0 0 0\n outer loop\n" + "".join(
            "  vertex %g %g %g\n" % v[i] for i in (a, b, c)) + " endloop\nendfacet\n")
    return "".join(out)


def main():
    slicer = sys.argv[1] if len(sys.argv) > 1 else DEFAULT
    with tempfile.TemporaryDirectory() as tmp:
        models = []
        for name, (w, d) in {"tile40": (40, 40), "tile20": (20, 20)}.items():
            path = os.path.join(tmp, name + ".stl")
            with open(path, "w") as fh:
                fh.write("solid %s\n%sendsolid %s\n" % (name, box(0, 0, w, d, 2.4), name))
            models.append(path)
        for out, extra in (("tiles_mini.bgcode", ["--binary-gcode"]), ("tiles_mini.gcode", [])):
            cmd = [slicer, "--export-gcode", "--merge"] + PROFILES + extra + models + \
                  ["--output", os.path.join(HERE, out)]
            if not extra:
                cmd.insert(3, "--binary-gcode=0")
            print(" ".join(cmd))
            subprocess.run(cmd, check=True)


if __name__ == "__main__":
    main()
