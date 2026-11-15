"""Times the kernels of `kernels.R` on GNU R and on ry's runtime.

Each engine runs the file `--runs` times in a fresh process, and the table shows the fastest run
per kernel with the speed-up over GNU R. A kernel whose checksum differs between the engines is
marked, and the exit status is then 1. Packages load from `R_LIBS`, which defaults to the
package-test corpus library (`just bench-packages install`).

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
    print("| kernel | GNU R (s) | ry (s) | speed-up | result |")
    print("|---|---:|---:|---:|---|")
    differing = 0
    for kernel, (gnu_seconds, gnu_checksum) in results["gnu"].items():
        ry_seconds, ry_checksum = results["ry"][kernel]
        same = gnu_checksum == ry_checksum
        differing += not same
        verdict = "same" if same else f"DIFFERS: GNU R `{gnu_checksum}`, ry `{ry_checksum}`"
        print(f"| {kernel} | {gnu_seconds:.3f} | {ry_seconds:.3f} | {gnu_seconds / ry_seconds:.2f}x | {verdict} |")
    sys.exit(1 if differing else 0)


def run(command, environment, runs):
    """Runs one engine and returns {kernel: (fastest seconds, checksum)}."""
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
            kernel, seconds, checksum = fields[0], float(fields[1]), fields[2]
            best = results.get(kernel, (seconds, checksum))[0]
            results[kernel] = (min(best, seconds), checksum)
    return results


if __name__ == "__main__":
    main()
