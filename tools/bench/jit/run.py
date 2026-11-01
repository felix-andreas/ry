#!/usr/bin/env python3
"""Runs the JIT benchmark suite under GNU R, ry's interpreter and ry's JIT, alternating engines
per benchmark so they never share the CPU, and prints a Markdown table of the best times.

Usage: tools/bench/jit/run.py [--ry PATH] [--filter NAME]
"""
import argparse, os, pathlib, subprocess, sys

here = pathlib.Path(__file__).resolve().parent
root = here.parent.parent.parent

def run(command, script, env):
    text = (here / "harness.R").read_text() + "\n" + script.read_text()
    scratch = here / ".scratch.R"
    scratch.write_text(text)
    try:
        output = subprocess.run(command + [str(scratch)], capture_output=True, text=True, env=env, timeout=1800)
    finally:
        scratch.unlink()
    lines = [line for line in output.stdout.splitlines() if line.strip()]
    if output.returncode != 0 or not lines:
        return None, (output.stderr.strip().splitlines() or ["failed"])[-1]
    name, checksum, seconds = lines[-1].split()
    return float(seconds), checksum

def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--ry", default=str(root / "target/release/ry"))
    parser.add_argument("--filter", default="")
    args = parser.parse_args()
    base = dict(os.environ, LC_ALL="C.UTF-8", R_DEFAULT_PACKAGES="stats")
    engines = [
        ("GNU R", ["Rscript", "--vanilla"], base),
        ("ry tier 0", [args.ry, "run", "--runtime", "ry"], dict(base, RY_JIT="off")),
        ("ry JIT", [args.ry, "run", "--runtime", "ry"], dict(base, RY_JIT="on")),
    ]
    print("| benchmark | " + " | ".join(name for name, _, _ in engines) + " | JIT vs GNU R |")
    print("| --- | " + " | ".join("---:" for _ in engines) + " | ---: |")
    for group in ["micro", "macro"]:
        for script in sorted((here / group).glob("*.R")):
            if args.filter not in script.stem:
                continue
            results = [run(command, script, env) for _, command, env in engines]
            checksums = {checksum for seconds, checksum in results if seconds is not None}
            cells = []
            for seconds, detail in results:
                cells.append(f"{seconds:.3f} s" if seconds is not None else f"fails ({detail[:40]})")
            ratio = ""
            if results[0][0] and results[2][0]:
                ratio = f"{results[0][0] / max(results[2][0], 1e-4):.1f}x"
            note = "" if len(checksums) <= 1 else " (checksums differ: " + ", ".join(sorted(checksums)) + ")"
            print(f"| {group}/{script.stem}{note} | " + " | ".join(cells) + f" | {ratio} |")
            sys.stdout.flush()

main()
