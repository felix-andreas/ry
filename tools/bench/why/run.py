#!/usr/bin/env python3
"""Measures the programs behind the benchmark table in docs/src/content/docs/why-ry.mdx: each one
untyped on GNU R, untyped on ry, and typed in strict mode on ry. Engines alternate per program so
they never share the CPU. Prints the machine, the R version and a Markdown table of best times.

Usage: tools/bench/why/run.py [--ry PATH] [--filter NAME]
"""
import argparse, os, pathlib, platform, subprocess, sys

here = pathlib.Path(__file__).resolve().parent
root = here.parent.parent.parent

def run(command, variant, program, env):
    # The scratch file sits beside the variant's ry.toml, so the typed run is checked in strict
    # mode, and `ry run` refuses to run it if strict mode finds an error.
    text = (here / variant / "harness.R").read_text() + "\n" + (here / variant / program).read_text()
    scratch = here / variant / ".scratch.R"
    scratch.write_text(text)
    try:
        output = subprocess.run(command + [str(scratch)], capture_output=True, text=True, env=env, timeout=1800)
    finally:
        scratch.unlink()
    lines = [line for line in output.stdout.splitlines() if line.strip()]
    if output.returncode != 0 or not lines:
        return None, ((output.stderr.strip() or output.stdout.strip()).splitlines() or ["failed"])[-1]
    _, checksum, seconds = lines[-1].split()
    return float(seconds), checksum

def machine():
    cpu = platform.processor() or platform.machine()
    try:
        for line in pathlib.Path("/proc/cpuinfo").read_text().splitlines():
            if line.startswith("model name"):
                cpu = line.split(":", 1)[1].strip()
                break
    except OSError:
        pass
    r_version = subprocess.run(["Rscript", "--version"], capture_output=True, text=True)
    version = (r_version.stdout + r_version.stderr).strip().splitlines()[0]
    return f"{cpu}, {os.cpu_count()} cores, {platform.system()} {platform.release()}; {version}"

def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--ry", default=str(root / "target/release/ry"))
    parser.add_argument("--filter", default="")
    args = parser.parse_args()
    base = dict(os.environ, LC_ALL="C.UTF-8", R_DEFAULT_PACKAGES="stats")
    ry = [args.ry, "run", "--runtime", "ry"]
    engines = [
        ("GNU R, untyped", ["Rscript", "--vanilla"], "untyped"),
        ("ry, untyped", ry, "untyped"),
        ("ry, typed and strict", ry, "typed"),
    ]
    print(f"Machine: {machine()}\n")
    print("| program | " + " | ".join(name for name, _, _ in engines) + " | ry vs GNU R |")
    print("| --- | " + " | ".join("---:" for _ in engines) + " | ---: |")
    for program in sorted((here / "untyped").glob("*.R")):
        if program.name == "harness.R" or args.filter not in program.stem:
            continue
        results = [run(command, variant, program.name, base) for _, command, variant in engines]
        checksums = {checksum for seconds, checksum in results if seconds is not None}
        cells = [f"{seconds:.3f} s" if seconds is not None else f"fails ({detail[:60]})" for seconds, detail in results]
        ratio = ""
        if results[0][0] and results[1][0]:
            ratio = f"{results[0][0] / max(results[1][0], 1e-4):.1f}x"
        note = "" if len(checksums) <= 1 else " (results differ: " + ", ".join(sorted(checksums)) + ")"
        print(f"| {program.stem}{note} | " + " | ".join(cells) + f" | {ratio} |")
        sys.stdout.flush()

main()
