#!/usr/bin/env python3
"""Measures the benchmark programs the docs show (docs/src/content/docs/benchmarks.mdx): each one
untyped on GNU R, untyped on ry, and typed in strict mode on ry. Engines alternate per program so
they never share the CPU. Prints the machine, the R version and a Markdown table of best warm times,
the time of each program's first, cold run (which includes compiling it), and peak resident memory; `--write` also stores them in results.json, which the docs pages and the
landing page render, and rewrites the short table in the two READMEs, which GitHub and the VS Code
Marketplace show as they are.

Usage: tools/bench/why/run.py [--ry PATH] [--filter NAME] [--repeat N] [--write]
"""
import argparse, json, os, pathlib, platform, re, subprocess, sys, tempfile

here = pathlib.Path(__file__).resolve().parent
root = here.parent.parent.parent
# The READMEs show these programs: the largest gain, a typical loop, and the two where ry gains least.
readme_programs = ["fib", "drawdown", "vector_arith", "log_rank"]
readmes = [root / "README.md", root / "editors/code/README.md"]

def run(command, variant, program, env):
    """The best warm time, the checksum, the peak memory and the cold time, or `None` and why it
    failed."""
    # The scratch file sits beside the variant's ry.toml, so the typed run is checked in strict
    # mode, and `ry run` refuses to run it if strict mode finds an error.
    text = (here / variant / "harness.R").read_text() + "\n" + (here / variant / program).read_text()
    scratch = here / variant / ".scratch.R"
    scratch.write_text(text)
    try:
        with tempfile.TemporaryFile("w+") as stdout, tempfile.TemporaryFile("w+") as stderr:
            child = subprocess.Popen(command + [str(scratch)], stdout=stdout, stderr=stderr, text=True, env=env)
            # wait4 reports this child's own peak resident set in KiB on Linux: the whole process
            # from startup on, and any process it waited for.
            _, status, usage = os.wait4(child.pid, 0)
            child.returncode = os.waitstatus_to_exitcode(status)
            stdout.seek(0)
            stderr.seek(0)
            out, err = stdout.read(), stderr.read()
    finally:
        scratch.unlink()
    lines = [line for line in out.splitlines() if line.strip()]
    if child.returncode != 0 or not lines:
        return None, ((err.strip() or out.strip()).splitlines() or ["failed"])[-1], None, None
    _, checksum, seconds, cold = lines[-1].split()
    return float(seconds), checksum, usage.ru_maxrss / 1024, float(cold)

def best(results):
    # Other load on the machine only ever slows a run down, so the fastest run is the time. Load
    # does not inflate memory, so the largest peak of any run is the memory, and no engine gets
    # credit for a lucky run.
    passed = [result for result in results if result[0] is not None]
    if passed:
        return (
            min(seconds for seconds, _, _, _ in passed),
            passed[0][1],
            max(peak for _, _, peak, _ in passed),
            min(cold for _, _, _, cold in passed),
        )
    return None, results[-1][1], None, None

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
    return f"{cpu}, {os.cpu_count()} cores, {platform.system()} {platform.release()}", version

def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--ry", default=str(root / "target/release/ry"))
    parser.add_argument("--filter", default="")
    parser.add_argument("--repeat", type=int, default=2, help="runs per program and engine; the best is kept")
    parser.add_argument("--write", action="store_true", help="store the results in results.json")
    args = parser.parse_args()
    base = dict(os.environ, LC_ALL="C.UTF-8", R_DEFAULT_PACKAGES="stats")
    ry = [args.ry, "run", "--runtime", "ry"]
    engines = [
        ("gnu_r", "GNU R, untyped", ["Rscript", "--vanilla"], "untyped"),
        ("ry", "ry, untyped", ry, "untyped"),
        ("ry_typed", "ry, typed and strict", ry, "typed"),
    ]
    cpu, r_version = machine()
    print(f"Machine: {cpu}; {r_version}\n")
    print("| program | " + " | ".join(name for _, name, _, _ in engines) + " | ry vs GNU R | "
          + " | ".join(f"{name}, cold" for _, name, _, _ in engines) + " | "
          + " | ".join(f"{name}, peak memory" for _, name, _, _ in engines) + " |")
    print("| --- |" + " ---: |" * (3 * len(engines) + 1))
    rows = []
    for program in sorted((here / "untyped").glob("*.R")):
        if program.name == "harness.R" or args.filter not in program.stem:
            continue
        results = [
            best([run(command, variant, program.name, base) for _ in range(args.repeat)])
            for _, _, command, variant in engines
        ]
        checksums = {checksum for seconds, checksum, _, _ in results if seconds is not None}
        cells = [f"{seconds:.3f} s" if seconds is not None else f"fails ({detail[:60]})" for seconds, detail, _, _ in results]
        colds = [f"{cold:.3f} s" if cold is not None else "" for _, _, _, cold in results]
        peaks = [f"{peak:.0f} MiB" if peak is not None else "" for _, _, peak, _ in results]
        ratio = ""
        if results[0][0] and results[1][0]:
            ratio = f"{results[0][0] / max(results[1][0], 1e-4):.1f}x"
        note = "" if len(checksums) <= 1 else " (results differ: " + ", ".join(sorted(checksums)) + ")"
        print(f"| {program.stem}{note} | " + " | ".join(cells) + f" | {ratio} | " + " | ".join(colds) + " | " + " | ".join(peaks) + " |")
        sys.stdout.flush()
        if note or any(seconds is None for seconds, _, _, _ in results):
            continue
        rows.append({
            "program": program.stem,
            **{key: round(seconds, 4) for (key, _, _, _), (seconds, _, _, _) in zip(engines, results)},
            "cold": {key: round(cold, 4) for (key, _, _, _), (_, _, _, cold) in zip(engines, results)},
            "peak_mib": {key: round(peak, 1) for (key, _, _, _), (_, _, peak, _) in zip(engines, results)},
        })
    if args.write:
        if args.filter:
            sys.exit("error: --write needs every program, so it cannot be combined with --filter")
        if len(rows) != len(list((here / "untyped").glob("*.R"))) - 1:
            sys.exit("error: a program failed or its engines disagree, so results.json was not written")
        results = {"machine": cpu, "r": r_version, "repeat": args.repeat, "programs": rows}
        (here / "results.json").write_text(json.dumps(results, indent=2) + "\n")
        print(f"\nwrote {here / 'results.json'}")
        table = readme_table(rows)
        for readme in readmes:
            text = readme.read_text()
            updated, count = re.subn(r"(<!-- bench-why -->\n).*?(<!-- /bench-why -->)", lambda match: match[1] + table + "\n" + match[2], text, flags=re.S)
            if count != 1:
                sys.exit(f"error: {readme} has no <!-- bench-why --> block to rewrite")
            readme.write_text(updated)
            print(f"wrote {readme}")

def readme_table(rows):
    by_name = {row["program"]: row for row in rows}
    lines = [
        "| program | GNU R | ry | ry, typed and strict | peak memory, GNU R / ry / typed |",
        "| --- | ---: | ---: | ---: | ---: |",
    ]
    for name in readme_programs:
        row = by_name[name]
        peak = row["peak_mib"]
        lines.append(
            f"| `{name}` | {row['gnu_r']:.3f} s | {row['ry']:.3f} s ({row['gnu_r'] / row['ry']:.1f}x) | {row['ry_typed']:.3f} s "
            f"| {peak['gnu_r']:.0f} / {peak['ry']:.0f} / {peak['ry_typed']:.0f} MiB |"
        )
    return "\n".join(lines)

main()
