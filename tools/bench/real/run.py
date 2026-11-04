#!/usr/bin/env python3
"""Times the representative workload mix (tools/bench/real/*.R) on GNU R and on ry's runtime with the
JIT on and off. Each workload runs in a fresh process per engine and repeat, engines alternate run by
run so they never share the CPU in a block, and each process reports the median of its warm runs
(see harness.R). The table shows the median over processes, the ratio to GNU R, and the geometric
mean over workloads. `--write` stores the results in results.json, the record the tables in benchmarks.mdx are copied from.

With `--suites DIR`, the generated benchmark packages under DIR (finbench, dsbench, biobench, from
`just bench-gen`) are timed too: their testthat suite end to end, once per engine and repeat.

With `--public SUITE` (repeatable, or `all`), the vendored public benchmark programs under
suites/SUITE/ are timed too (see suites/README.md). Each program runs once per process from its own
directory and reports the seconds of that one run, so it includes GNU R's byte compilation and ry's
tier-up. Their rows are labeled `SUITE/NAME` and get a geometric mean per suite. `--fastr PATH` adds
FastR's Rscript as one more engine. Neither can be combined with `--write`, so results.json keeps
the workload mix only.

Usage: tools/bench/real/run.py [--ry PATH] [--filter NAME] [--repeat N] [--suites DIR]
                               [--public SUITE]... [--fastr PATH] [--write]
"""
import argparse, json, math, os, pathlib, platform, statistics, subprocess, sys, tempfile, time

here = pathlib.Path(__file__).resolve().parent
root = here.parent.parent.parent
# A program that is quadratic on one engine must not stall the whole table.
PROGRAM_TIMEOUT = 300

def engines(ry, fastr):
    base = dict(os.environ, LC_ALL="C.UTF-8")
    base.pop("RY_JIT", None)
    command = [ry, "run", "--runtime", "ry"]
    table = [
        ("gnu_r", "GNU R", ["Rscript", "--vanilla"], base),
        ("ry_jit", "ry, JIT on", command, base),
        ("ry_nojit", "ry, JIT off", command, dict(base, RY_JIT="off")),
    ]
    if fastr:
        table.append(("fastr", "FastR", [fastr, "--vanilla"], base))
    return table

def run_workload(command, env, program):
    """The median and best warm seconds, the cold seconds and the checksum, or `None` and why."""
    text = (here / "harness.R").read_text() + "\n" + program.read_text()
    with tempfile.NamedTemporaryFile("w", suffix=".R", dir=here, prefix=".scratch-", delete=False) as scratch:
        scratch.write(text)
    try:
        child = subprocess.run(command + [scratch.name], capture_output=True, text=True, env=env)
    finally:
        os.unlink(scratch.name)
    lines = [line for line in child.stdout.splitlines() if line.startswith(program.stem + " ")]
    if child.returncode != 0 or not lines:
        detail = (child.stderr.strip() or child.stdout.strip() or "failed").splitlines()[-1]
        return None, detail
    _, checksum, median, best, cold = lines[-1].split()
    return {"median": float(median), "best": float(best), "cold": float(cold)}, checksum

def run_program(command, env, program):
    """Seconds for one run of a vendored public program and its checksum, or `None` and why."""
    try:
        child = subprocess.run(command + [program.name], capture_output=True, text=True, env=env,
                               cwd=program.parent, timeout=PROGRAM_TIMEOUT)
    except subprocess.TimeoutExpired:
        return None, f"timed out after {PROGRAM_TIMEOUT} s"
    lines = [line for line in child.stdout.splitlines() if line.startswith(program.stem + " ")]
    if child.returncode != 0 or not lines:
        detail = (child.stderr.strip() or child.stdout.strip() or "failed").splitlines()[-1]
        return None, detail
    _, checksum, seconds = lines[-1].split()
    return {"median": float(seconds), "best": float(seconds), "cold": float(seconds)}, checksum

def run_suite(command, env, package):
    """Wall seconds for the package's whole testthat suite, or `None` and why."""
    script = root / "tools/bench/run-tests.R"
    started = time.monotonic()
    child = subprocess.run(command + [str(script), str(package), "silent"], capture_output=True, text=True, env=env)
    seconds = time.monotonic() - started
    summary = [line for line in child.stdout.splitlines() if " tests, " in line]
    if not summary:
        return None, ((child.stderr.strip() or "failed").splitlines() or ["failed"])[-1]
    return {"median": seconds, "best": seconds, "cold": seconds}, summary[-1].split(": ", 1)[-1]

def machine():
    cpu = platform.processor() or platform.machine()
    try:
        for line in pathlib.Path("/proc/cpuinfo").read_text().splitlines():
            if line.startswith("model name"):
                cpu = line.split(":", 1)[1].strip()
                break
    except OSError:
        pass
    version = subprocess.run(["Rscript", "--version"], capture_output=True, text=True)
    return f"{cpu}, {os.cpu_count()} cores", (version.stdout + version.stderr).strip().splitlines()[0]

def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--ry", default=str(root / "target/release/ry"))
    parser.add_argument("--filter", default="")
    parser.add_argument("--repeat", type=int, default=3, help="processes per workload and engine")
    parser.add_argument("--suites", help="directory holding generated benchmark packages")
    parser.add_argument("--public", action="append", default=[], metavar="SUITE",
                        help="vendored public suite under suites/ to time too (repeatable, or `all`)")
    parser.add_argument("--fastr", metavar="PATH", help="FastR's Rscript, timed as one more engine")
    parser.add_argument("--write", action="store_true")
    args = parser.parse_args()
    if args.write and (args.public or args.fastr):
        sys.exit("error: --write stores the workload mix only, so it cannot be combined with --public or --fastr")
    table = engines(args.ry, args.fastr)
    cpu, r_version = machine()
    print(f"Machine: {cpu}; {r_version}\n")
    print("| workload | " + " | ".join(name for _, name, _, _ in table) + " | GNU R / ry JIT on | GNU R / ry JIT off |")
    print("| --- |" + " ---: |" * (len(table) + 2))
    jobs = [(p.stem, "workload", p) for p in sorted(here.glob("*.R")) if p.name != "harness.R" and args.filter in p.stem]
    if args.suites:
        for name in ("finbench", "dsbench", "biobench"):
            package = pathlib.Path(args.suites) / name
            if package.is_dir() and args.filter in name:
                jobs.append((f"{name} tests", "suite", package))
    available = sorted(path.name for path in (here / "suites").iterdir() if path.is_dir())
    for suite in available if "all" in args.public else args.public:
        if suite not in available:
            sys.exit(f"error: no public suite {suite!r}; the suites are {', '.join(available)}")
        for program in sorted((here / "suites" / suite).glob("*.R")):
            if args.filter in f"{suite}/{program.stem}":
                jobs.append((f"{suite}/{program.stem}", "program", program))
    runners = {"workload": run_workload, "program": run_program, "suite": run_suite}
    rows = []
    for name, kind, target in jobs:
        samples = {key: [] for key, _, _, _ in table}
        checks = {}
        failure = {}
        for _ in range(args.repeat):
            for key, _, command, env in table:
                result, detail = runners[kind](command, env, target)
                if result is None:
                    failure[key] = detail
                else:
                    samples[key].append(result)
                    checks.setdefault(key, detail)
        medians = {key: statistics.median(s["median"] for s in values) if values else None for key, values in samples.items()}
        cells = [f"{medians[key]:.3f} s" if medians[key] else f"fails ({failure.get(key, '')[:50]})" for key, _, _, _ in table]
        ratios = [f"{medians['gnu_r'] / medians[key]:.2f}x" if medians["gnu_r"] and medians[key] else "" for key in ("ry_jit", "ry_nojit")]
        agree = kind == "suite" or len(set(checks.values())) <= 1
        note = "" if agree else " (results differ)"
        print(f"| {name}{note} | " + " | ".join(cells) + " | " + " | ".join(ratios) + " |")
        sys.stdout.flush()
        if agree and all(medians[key] for key in ("gnu_r", "ry_jit", "ry_nojit")):
            rows.append({
                "workload": name, "kind": kind,
                **{key: round(value, 4) for key, value in medians.items() if value},
                "cold": {key: round(min(s["cold"] for s in values), 4) for key, values in samples.items() if values},
            })
    def geomean(group, key):
        return math.exp(statistics.fmean(math.log(row["gnu_r"] / row[key]) for row in group))
    groups = [("workloads", [row for row in rows if row["kind"] == "workload"])]
    for suite in sorted({row["workload"].split("/")[0] for row in rows if row["kind"] == "program"}):
        groups.append((f"{suite} programs", [row for row in rows if row["workload"].startswith(suite + "/")]))
    for label, group in groups:
        if group:
            summary = f"JIT on {geomean(group, 'ry_jit'):.2f}x, JIT off {geomean(group, 'ry_nojit'):.2f}x"
            measured = [row for row in group if row.get("fastr")]
            if measured:
                summary += f", FastR {geomean(measured, 'fastr'):.2f}x ({len(measured)} of them)"
            print(f"\nGeometric mean speed-up over GNU R ({len(group)} {label}): {summary}")
    if args.write:
        if args.filter:
            sys.exit("error: --write needs every workload, so it cannot be combined with --filter")
        results = {"machine": cpu, "r": r_version, "repeat": args.repeat, "workloads": rows}
        (here / "results.json").write_text(json.dumps(results, indent=2) + "\n")
        print(f"wrote {here / 'results.json'}")

main()
