#!/usr/bin/env python3
"""Times the representative workload mix (tools/bench/real/*.R) on GNU R and on ry's runtime with the
JIT on and off. Each workload runs in a fresh process per engine and round, and each process reports
the median of its warm runs (see harness.R).

A round runs every engine once, in an order shuffled per round from `--seed`. The table shows each
engine's median seconds over the rounds and, for each pair of engines compared, the median of the
per-round ratios with the lowest and highest round beside it. Pairing within a round cancels a drift
of the machine's speed that lasts longer than one round. A row whose median on any engine is under
0.1 s is marked too short and left out of the geometric means, which are printed with a 95%
bootstrap interval over rounds. `--baseline PATH` times a second ry build (JIT on and off) in the
same rounds, so both builds are compared with the same GNU R samples and with each other.
`--write` stores the results in results.json, which the tables on the docs site render, and
rewrites the summary table in README.md.

With `--suites DIR`, the generated benchmark packages under DIR (finbench, dsbench, biobench, from
`just bench-gen`) are timed too: their testthat suite end to end, once per engine and round.

With `--public SUITE` (repeatable, or `all`), the vendored public benchmark programs under
suites/SUITE/ are timed too (see suites/README.md). Each program runs once per process from its own
directory and reports the seconds of that one run, so it includes GNU R's byte compilation and ry's
tier-up. Their rows are labeled `SUITE/NAME` and get a geometric mean per suite. `--fastr PATH` adds
FastR's Rscript as one more engine. Neither can be combined with `--write`, so results.json keeps
the workload mix only.

Usage: tools/bench/real/run.py [--ry PATH] [--baseline PATH] [--filter NAME] [--repeat N] [--seed N]
                               [--suites DIR] [--public SUITE]... [--fastr PATH] [--write]
"""
import argparse, json, math, os, pathlib, platform, random, statistics, subprocess, sys, tempfile, time

here = pathlib.Path(__file__).resolve().parent
root = here.parent.parent.parent
# A program that is quadratic on one engine must not stall the whole table.
PROGRAM_TIMEOUT = 300
# On a shared cloud VM a fixed C loop varies by about 25% between processes, so three samples
# cannot tell a 10% change from noise. Five is the fewest the paired ratios need.
MINIMUM_REPEAT = 5
# Below this many seconds on any engine, a row's ratio mostly measures start-up and timer noise.
SHORT = 0.1

def engines(ry, baseline, fastr):
    base = dict(os.environ, LC_ALL="C.UTF-8")
    base.pop("RY_JIT", None)
    command = [ry, "run", "--runtime", "ry"]
    table = [
        ("gnu_r", "GNU R", ["Rscript", "--vanilla"], base),
        ("ry_jit", "ry, JIT on", command, base),
        ("ry_nojit", "ry, JIT off", command, dict(base, RY_JIT="off")),
    ]
    if baseline:
        command = [baseline, "run", "--runtime", "ry"]
        table.append(("baseline_jit", "baseline, JIT on", command, base))
        table.append(("baseline_nojit", "baseline, JIT off", command, dict(base, RY_JIT="off")))
    if fastr:
        table.append(("fastr", "FastR", [fastr, "--vanilla"], base))
    return table

def run_workload(command, env, program):
    """The process's median warm seconds and the checksum, or `None` and why."""
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
    _, checksum, median, _, _ = lines[-1].split()
    return float(median), checksum

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
    return float(seconds), checksum

def run_suite(command, env, package):
    """Wall seconds for the package's whole testthat suite, or `None` and why."""
    script = root / "tools/bench/run-tests.R"
    started = time.monotonic()
    child = subprocess.run(command + [str(script), str(package), "silent"], capture_output=True, text=True, env=env)
    seconds = time.monotonic() - started
    summary = [line for line in child.stdout.splitlines() if " tests, " in line]
    if not summary:
        return None, ((child.stderr.strip() or "failed").splitlines() or ["failed"])[-1]
    return seconds, summary[-1].split(": ", 1)[-1]

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
    parser.add_argument("--repeat", type=int, default=MINIMUM_REPEAT, help="rounds; each runs every engine once")
    parser.add_argument("--baseline", metavar="PATH", help="a second ry, timed in the same rounds")
    parser.add_argument("--seed", type=int, default=0, help="seed of the engine order and the bootstrap")
    parser.add_argument("--suites", help="directory holding generated benchmark packages")
    parser.add_argument("--public", action="append", default=[], metavar="SUITE",
                        help="vendored public suite under suites/ to time too (repeatable, or `all`)")
    parser.add_argument("--fastr", metavar="PATH", help="FastR's Rscript, timed as one more engine")
    parser.add_argument("--write", action="store_true")
    args = parser.parse_args()
    if args.write and (args.public or args.fastr):
        sys.exit("error: --write stores the workload mix only, so it cannot be combined with --public or --fastr")
    if args.repeat < MINIMUM_REPEAT:
        sys.exit(f"error: --repeat must be at least {MINIMUM_REPEAT}; fewer rounds cannot separate a change from noise")
    if args.write and args.baseline:
        sys.exit("error: --write records one build, so it cannot be combined with --baseline")
    table = engines(args.ry, args.baseline, args.fastr)
    titles = {key: name for key, name, _, _ in table}
    keys = [key for key, _, _, _ in table]
    compared = [("gnu_r", key) for key in keys if key != "gnu_r"]
    if args.baseline:
        compared += [("baseline_jit", "ry_jit"), ("baseline_nojit", "ry_nojit")]
    cpu, r_version = machine()
    print(f"Machine: {cpu}; {r_version}\n")
    print(f"Each ratio is the median over {args.repeat} rounds of the two engines' times in the same round, "
          "with the lowest and highest round in parentheses. Above 1 means the second engine is faster.\n")
    print("| workload | " + " | ".join(name for _, name, _, _ in table) + " | "
          + " | ".join(f"{titles[a]} / {titles[b]}" for a, b in compared) + " |")
    print("| --- |" + " ---: |" * (len(table) + len(compared)))
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
    shuffle = random.Random(args.seed)
    rows = []
    for name, kind, target in jobs:
        rounds = []
        checks = {}
        failure = {}
        for _ in range(args.repeat):
            # A fresh engine order each round, so no engine always runs right after another one
            # and inherits its cache and frequency state.
            order = list(table)
            shuffle.shuffle(order)
            measured = {}
            for key, _, command, env in order:
                result, detail = runners[kind](command, env, target)
                if result is None:
                    failure[key] = detail
                else:
                    measured[key] = result
                    checks.setdefault(key, detail)
            rounds.append(measured)
        samples = {key: [r[key] for r in rounds if key in r] for key, _, _, _ in table}
        medians = {key: statistics.median(values) if values else None for key, values in samples.items()}
        ratios = {f"{numerator}/{key}": paired(rounds, numerator, key) for numerator, key in compared}
        short = any(value is not None and value < SHORT for value in medians.values())
        cells = [f"{medians[key]:.3f} s" if medians[key] else f"fails ({failure.get(key, '')[:50]})" for key, _, _, _ in table]
        cells += [spread(ratios[f"{numerator}/{key}"]) for numerator, key in compared]
        agree = kind == "suite" or len(set(checks.values())) <= 1
        note = "" if agree else " (results differ)"
        if short:
            note += " (too short, left out of the mean)"
        print(f"| {name}{note} | " + " | ".join(cells) + " |")
        sys.stdout.flush()
        if agree and all(medians[key] for key, _, _, _ in table):
            rows.append({
                "workload": name, "kind": kind, "short": short,
                **{key: round(value, 4) for key, value in medians.items() if value},
                "ratios": {key: [round(value, 4) for value in values] for key, values in ratios.items()},
            })
    groups = [("workloads", [row for row in rows if row["kind"] == "workload"])]
    for suite in sorted({row["workload"].split("/")[0] for row in rows if row["kind"] == "program"}):
        groups.append((f"{suite} programs", [row for row in rows if row["workload"].startswith(suite + "/")]))
    for label, group in groups:
        timed = [row for row in group if not row["short"]]
        if not timed:
            continue
        left_out = len(group) - len(timed)
        print(f"\nGeometric mean over {len(timed)} {label}" + (f" ({left_out} too short, left out)" if left_out else "") + ":")
        for numerator, key in compared:
            point, low, high = bootstrap([row["ratios"][f"{numerator}/{key}"] for row in timed], args.seed)
            print(f"  {titles[numerator]} / {titles[key]}: {point:.2f}x (95% interval {low:.2f} to {high:.2f})")
    if args.write:
        if args.filter:
            sys.exit("error: --write needs every workload, so it cannot be combined with --filter")
        results = {"date": time.strftime("%Y-%m-%d"), "commit": commit(), "machine": cpu, "r": r_version, "repeat": args.repeat, "workloads": rows}
        (here / "results.json").write_text(json.dumps(results, indent=2) + "\n")
        print(f"wrote {here / 'results.json'}")
        subprocess.run([sys.executable, "-I", str(root / "tools/bench/readme.py")], check=True)

def commit():
    """The ry commit measured, marked when the working tree has changes."""
    head = subprocess.run(["git", "-C", str(root), "rev-parse", "--short", "HEAD"], capture_output=True, text=True, check=True).stdout.strip()
    dirty = subprocess.run(["git", "-C", str(root), "status", "--porcelain", "--untracked-files=no"], capture_output=True, text=True, check=True).stdout.strip()
    return head + ("-dirty" if dirty else "")

def paired(rounds, numerator, denominator):
    """The ratio of the two engines' times within each round in which both ran, sorted. Pairing
    within a round cancels a drift in the machine's speed that lasts longer than one round."""
    return sorted(r[numerator] / r[denominator] for r in rounds if numerator in r and denominator in r)

def spread(ratios):
    if not ratios:
        return ""
    return f"{statistics.median(ratios):.2f}x ({ratios[0]:.2f} to {ratios[-1]:.2f})"

def bootstrap(per_row, seed, resamples=2000):
    """The geometric mean of each row's median paired ratio, and a 95% percentile bootstrap
    interval that resamples each row's rounds."""
    point = geomean(statistics.median(ratios) for ratios in per_row)
    draw = random.Random(seed)
    means = sorted(
        geomean(statistics.median(draw.choices(ratios, k=len(ratios))) for ratios in per_row)
        for _ in range(resamples)
    )
    return point, means[int(0.025 * resamples)], means[int(0.975 * resamples) - 1]

def geomean(ratios):
    return math.exp(statistics.fmean(math.log(ratio) for ratio in ratios))

if __name__ == "__main__":
    main()
