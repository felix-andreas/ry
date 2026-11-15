"""Times the kernels of `kernels.R` on GNU R and on ry's runtime.

Each engine runs the file `--runs` times in a fresh process, and the table shows the fastest run
per kernel with the speed-up over GNU R. A kernel whose checksum differs between the engines or
between two runs, or that one engine did not print, is marked, and the exit status is then 1.
Packages load from `R_LIBS`, which defaults to the package-test corpus library
(`just bench-packages install`).

Usage: python3 -I tools/bench/internal/ci/run.py [--only k1,k3] [--runs 2] [--scale 1] [--ry PATH]
"""

import argparse
import os
import subprocess
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.dirname(HERE))))
SCRIPT = os.path.join(HERE, "kernels.R")


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--only", default="", help="comma-separated kernels, e.g. k1,k3")
    parser.add_argument("--runs", type=int, default=2)
    parser.add_argument("--scale", default="1", help="multiplies every kernel's size")
    parser.add_argument("--ry", default=os.path.join(ROOT, "target", "release", "ry"))
    parser.add_argument("--rscript", default=os.environ.get("ORACLE_R_RSCRIPT", "Rscript"))
    arguments = parser.parse_args()
    environment = dict(os.environ, KERNELS=arguments.only, SCALE=arguments.scale)
    environment.setdefault("R_LIBS", os.path.join(ROOT, ".corpus", "packages", "library"))
    engines = {
        "gnu": [arguments.rscript, SCRIPT],
        "ry": [arguments.ry, "run", "--runtime", "ry", SCRIPT],
    }
    results = {engine: run(command, environment, arguments.runs) for engine, command in engines.items()}
    kernels = list(results["gnu"]) + [kernel for kernel in results["ry"] if kernel not in results["gnu"]]
    if not kernels:
        raise SystemExit("neither engine printed a kernel")
    print("| kernel | GNU R (s) | ry (s) | speed-up | result |")
    print("|---|---:|---:|---:|---|")
    failures = 0
    for kernel in kernels:
        if kernel not in results["gnu"] or kernel not in results["ry"]:
            absent = "GNU R" if kernel not in results["gnu"] else "ry"
            print(f"| {kernel} | | | | missing on {absent} |")
            failures += 1
            continue
        gnu_seconds, gnu_checksums = results["gnu"][kernel]
        ry_seconds, ry_checksums = results["ry"][kernel]
        same = gnu_checksums == ry_checksums and len(gnu_checksums) == 1
        failures += not same
        verdict = "same" if same else f"DIFFERS: GNU R {sorted(gnu_checksums)}, ry {sorted(ry_checksums)}"
        speedup = f"{gnu_seconds / ry_seconds:.2f}x" if ry_seconds > 0 else ""
        print(f"| {kernel} | {gnu_seconds:.3f} | {ry_seconds:.3f} | {speedup} | {verdict} |")
    sys.exit(1 if failures else 0)


def run(command, environment, runs):
    """Runs one engine and returns {kernel: (fastest seconds, the checksums its runs printed)}."""
    results = {}
    for _ in range(runs):
        completed = subprocess.run(command, capture_output=True, text=True, env=environment)
        if completed.returncode != 0:
            sys.stderr.write(completed.stdout + completed.stderr)
            raise SystemExit(f"{' '.join(command)} failed")
        for line in completed.stdout.splitlines():
            fields = line.split("\t")
            if len(fields) != 3:
                continue
            try:
                seconds = float(fields[1])
            except ValueError:
                continue
            best, checksums = results.get(fields[0], (seconds, set()))
            results[fields[0]] = (min(best, seconds), checksums | {fields[2]})
    return results


if __name__ == "__main__":
    main()
