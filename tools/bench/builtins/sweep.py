#!/usr/bin/env python3
"""The builtins sweep: every exported function of base, stats and utils, timed on GNU R and on ry.

The cases live in cases/*.R and the harness in harness.R, which documents their format. bench.py
runs the sweep as its `builtins` suite and writes results.json from it; this script lists the
cases, times them for a quick look, and keeps the ratchet.

The ratchet is slower.txt: the functions known to be slower on ry than on GNU R, or failing on ry,
one `package::function` per line. The list may only shrink. A function is slower when any of its
cases is, on ry with the JIT or with it off: its ratio, GNU R's time per call over ry's, has a
median over rounds below 1.

Usage:
  sweep.py list                       count the cases and skips, and name the exports neither covers
  sweep.py check                      the fast static check: every export covered, and slower.txt
                                      sorted, naming known functions, and no longer than on main
  sweep.py compare [--filter REGEX] [--rounds N] [--jit-off] [--ry PATH]
                                      time the cases on GNU R and ry and print the ratios, worst first
  sweep.py ratchet [--ci] [--update] [--seed] [--tolerance T] [--ry PATH]
                                      time the sweep and fail when a function not in slower.txt is
                                      slower than GNU R by more than the tolerance; --update removes
                                      the listed functions that are no longer slower; --seed rewrites
                                      the list as every function slower now, which only a change to
                                      the cases justifies (`check` refuses a list that grew)
  sweep.py validate [--filter REGEX] [--with-ry]
                                      run the cases once on GNU R, quickly, and report the cases that
                                      fail, warn or take over a second per call; with --with-ry, also
                                      run them on ry and report its failures and differing results
  sweep.py weights                    each function's share of calls in the code-base templates and
                                      the oracle corpus, the order fixes are worked in

`--ci` times only the cases without a 1e6 input, in shorter batches, and gives every function that
looks slower two more rounds before judging it, so it runs in a few minutes.
"""
import argparse, collections, concurrent.futures, math, os, pathlib, re, shutil, statistics, subprocess, sys, tempfile, threading, time

here = pathlib.Path(__file__).resolve().parent
root = here.parent.parent.parent
CASES = here / "cases"
SLOWER = here / "slower.txt"
HARNESS = here / "harness.R"
REPLAY = here.parent / "hot" / "replay.R"
# A case that prints no line for this long is stopped and recorded as timed out, and the process
# restarts after it. The first case of a process also waits for the fixtures it names to be built.
CASE_TIMEOUT = 180
# The variants a CI run leaves out: their inputs (1e5 or more elements, or the 1000x1000 matrix)
# take GNU R a good part of a second to build, and their calls take milliseconds each.
LARGE = re.compile(r"1e[5-9]|mat_1000")


def script(work):
    """The harness after hot/replay.R, whose `.hot$checksum` it uses, written into `work`."""
    path = pathlib.Path(work) / "sweep.R"
    path.write_text(REPLAY.read_text() + "\n" + HARNESS.read_text())
    return path


def list_cases(rscript="Rscript"):
    """The cases, skips, uncovered exports and unknown names, as listed by the harness on GNU R."""
    with tempfile.TemporaryDirectory(prefix="ry-sweep-") as work:
        out = pathlib.Path(work) / "list.tsv"
        env = dict(os.environ, RY_SWEEP_DIR=str(CASES), RY_SWEEP_MODE="list", RY_SWEEP_OUT=str(out), LC_ALL="C.UTF-8")
        completed = subprocess.run([rscript, "--vanilla", str(script(work))], env=env, cwd=work,
                                   capture_output=True, text=True)
        if completed.returncode != 0:
            sys.exit(f"error: listing the sweep's cases failed:\n{completed.stderr.strip()}")
        listing = {"cases": [], "skips": [], "missing": [], "unknown": [], "broken": []}
        for line in out.read_text().splitlines():
            kind, *fields = line.split("\t")
            if kind == "case":
                id_, package, name, variant, display = fields
                listing["cases"].append({"id": id_, "package": package, "function": name, "variant": variant,
                                         "display": display})
            elif kind == "skip":
                package, name, reason = fields
                listing["skips"].append({"package": package, "function": name, "reason": reason})
            elif kind == "missing":
                listing["missing"].append("::".join(fields))
            elif kind == "unknown":
                listing["unknown"].append(fields[0])
            elif kind == "broken":
                listing["broken"].append(": ".join(fields))
        return listing


def function_of(case_id):
    """`base::abs` for the case `base::abs/dbl_1e6`."""
    return case_id.rsplit("/", 1)[0]


def run_cases(command, env, ids, batch=0.01, batches=5, case_timeout=CASE_TIMEOUT, progress=None):
    """Times the cases `ids` in harness processes started with `command` (the engine's command line,
    which the script's path is appended to) and returns `({id: result}, stderr)`. A result is
    `{"checksum", "seconds", "min", "calls"}`, with the median and the fastest seconds per call, or
    `{"seconds": None, "reason"}`. A process that dies or stops printing loses only the case it was
    in: that case is recorded as failed and a new process times the rest."""
    results, errors = {}, []
    remaining = list(ids)
    with tempfile.TemporaryDirectory(prefix="ry-sweep-") as work:
        work = pathlib.Path(work)
        path = script(work)
        attempt = 0
        while remaining:
            attempt += 1
            table = work / f"cases{attempt}.txt"
            table.write_text("\n".join(remaining) + "\n")
            out = work / f"out{attempt}.tsv"
            out.touch()
            variables = dict(env, RY_SWEEP_DIR=str(CASES), RY_SWEEP_MODE="time", RY_SWEEP_CASES=str(table),
                             RY_SWEEP_OUT=str(out), RY_SWEEP_BATCH=str(batch), RY_SWEEP_BATCHES=str(batches))
            with open(work / f"err{attempt}.txt", "w+") as stderr:
                child = subprocess.Popen([*command, str(path)], env=variables, cwd=work,
                                         stdout=subprocess.DEVNULL, stderr=stderr)
                seen, last_change, timed_out = 0, time.monotonic(), False
                while True:
                    status = child.poll()
                    lines = complete_lines(out)
                    if len(lines) > seen:
                        for line in lines[seen:]:
                            parsed = parse_line(line)
                            if parsed:
                                results[parsed[0]] = parsed[1]
                                if progress:
                                    progress(parsed[0], parsed[1])
                        seen, last_change = len(lines), time.monotonic()
                    if status is not None:
                        break
                    if time.monotonic() - last_change > case_timeout:
                        child.kill()
                        child.wait()
                        timed_out = True
                        break
                    time.sleep(0.05)
                for line in complete_lines(out)[seen:]:
                    if (parsed := parse_line(line)):
                        results[parsed[0]] = parsed[1]
                stderr.seek(0)
                text = stderr.read()
            errors.append(text)
            left = [case for case in remaining if case not in results]
            if not left:
                break
            culprit = left[0]
            if timed_out:
                reason = f"timed out: no result after {case_timeout} s"
            else:
                last = (text.strip().splitlines() or [f"exit status {child.returncode}"])[-1][:300]
                reason = f"the process ended (status {child.returncode}): {last}"
            results[culprit] = {"seconds": None, "reason": reason}
            if progress:
                progress(culprit, results[culprit])
            remaining = left[1:]
    return results, "\n".join(errors)


def complete_lines(path):
    text = path.read_text(errors="replace")
    return text.split("\n")[:-1]


def parse_line(line):
    fields = line.split("\t")
    if len(fields) >= 3 and fields[1] == "failed":
        return fields[0], {"seconds": None, "reason": "\t".join(fields[2:])}
    if len(fields) != 5:
        return None
    id_, checksum, least, median, calls = fields
    try:
        return id_, {"checksum": checksum, "seconds": float(median), "min": float(least), "calls": int(float(calls))}
    except ValueError:
        return None


# Engines.

def engines(ry, rscript, jit_off=True):
    base = {key: value for key, value in os.environ.items() if key not in ("RY_JIT", "RY_TYPED_LOG")}
    base["LC_ALL"] = "C.UTF-8"
    table = {"gnu_r": ([rscript, "--vanilla"], base), "ry": ([ry, "run", "--runtime", "ry"], base)}
    if jit_off:
        table["ry_nojit"] = ([ry, "run", "--runtime", "ry"], dict(base, RY_JIT="off"))
    return table


def measure(ids, table, rounds, batch, batches, verbose=True, parallel=False):
    """`{engine: [ {id: result} per round ]}`, the engines taking turns within each round, or with
    `parallel` running side by side on separate cores, which is faster and noisier."""
    measured = {engine: [] for engine in table}
    for number in range(rounds):
        def one(engine):
            command, env = table[engine]
            started = time.monotonic()
            done = collections.Counter()

            def progress(case, result):
                done["cases"] += 1
                if result.get("seconds") is None:
                    done["failed"] += 1
                if verbose and done["cases"] % 200 == 0:
                    print(f"    {engine}: {done['cases']} of {len(ids)} cases", file=sys.stderr, flush=True)

            results, _ = run_cases(command, env, ids, batch=batch, batches=batches, progress=progress)
            if verbose:
                print(f"  round {number + 1}, {engine}: {len(ids)} cases in {time.monotonic() - started:.0f} s, "
                      f"{done['failed']} failed", file=sys.stderr, flush=True)
            return results

        if parallel:
            with concurrent.futures.ThreadPoolExecutor(len(table)) as pool:
                rounds_ = dict(zip(table, pool.map(one, table)))
        else:
            rounds_ = {engine: one(engine) for engine in table}
        for engine in table:
            measured[engine].append(rounds_[engine])
    return measured


def case_ratios(measured, ids):
    """`{id: {engine: ratio or None}}`: GNU R's time per call over the engine's, the median of
    the per-round ratios, or None where the engine failed the case. Also `{id: reason}` for the
    cases GNU R itself fails, which are broken cases rather than measurements."""
    ratios, broken = {}, {}
    for case in ids:
        gnu = [round_[case] for round_ in measured["gnu_r"] if case in round_]
        if not gnu or any(run["seconds"] is None for run in gnu):
            broken[case] = next((run["reason"] for run in gnu if run["seconds"] is None), "not run")
            continue
        ratios[case] = {}
        for engine, rounds in measured.items():
            if engine == "gnu_r":
                continue
            per_round = []
            for gnu_round, round_ in zip(measured["gnu_r"], rounds):
                run = round_.get(case)
                if run is None or run["seconds"] is None:
                    per_round = None
                    break
                per_round.append(gnu_round[case]["seconds"] / max(run["seconds"], 1e-12))
            ratios[case][engine] = statistics.median(per_round) if per_round else None
    return ratios, broken


def function_ratios(ratios):
    """`{function: (worst ratio or None for a failure, the case and engine it comes from)}`."""
    worst = {}
    for case, by_engine in ratios.items():
        name = function_of(case)
        for engine, ratio in by_engine.items():
            value = -math.inf if ratio is None else ratio
            if name not in worst or value < worst[name][0]:
                worst[name] = (value, case, engine)
    return {name: (None if value == -math.inf else value, case, engine) for name, (value, case, engine) in worst.items()}


def read_list(path=SLOWER):
    if not path.exists():
        return []
    return [line.strip() for line in path.read_text().splitlines() if line.strip() and not line.startswith("#")]


LIST_HEADER = """\
# The exported functions of base, stats and utils that are slower on ry than on GNU R, or fail on ry,
# in at least one case of the builtins sweep (tools/bench/builtins/sweep.py). The list may only
# shrink: `sweep.py ratchet` fails on a function that is slower and not listed, and `sweep.py check`
# fails when this file names a function that main's copy does not.
"""


def write_list(names, path=SLOWER):
    path.write_text(LIST_HEADER + "".join(f"{name}\n" for name in sorted(names)))


# Commands.

def command_list(args):
    listing = list_cases(args.rscript)
    functions = {function_of(case["id"]) for case in listing["cases"]}
    print(f"{len(listing['cases'])} cases of {len(functions)} functions, {len(listing['skips'])} skipped functions")
    for problem in listing["broken"]:
        print(f"broken: {problem}")
    for name in listing["unknown"]:
        print(f"unknown: {name} is not an exported function")
    for name in listing["missing"]:
        print(f"missing: {name}")
    return 1 if listing["missing"] or listing["unknown"] or listing["broken"] else 0


def command_check(args):
    """The static check: no measurement, a few seconds."""
    problems = []
    listing = list_cases(args.rscript)
    problems += [f"{name} has neither a case nor a skip" for name in listing["missing"]]
    problems += [f"{name} is not an exported function of its package" for name in listing["unknown"]]
    problems += [f"a case file does not load: {problem}" for problem in listing["broken"]]
    benched = {function_of(case["id"]) for case in listing["cases"]}
    lines = read_list()
    if lines != sorted(set(lines)):
        problems.append("slower.txt is not sorted or has duplicates")
    problems += [f"slower.txt names {name}, which has no case" for name in lines if name not in benched]
    previous = list_on_main()
    if previous is None:
        print("note: no origin/main to compare slower.txt with")
    else:
        problems += [f"slower.txt adds {name}, which main does not list; make it faster instead"
                     for name in sorted(set(lines) - set(previous))]
    for problem in problems:
        print(f"error: {problem}")
    if not problems:
        print(f"ok: {len(benched)} functions in {len(listing['cases'])} cases, {len(listing['skips'])} skipped, "
              f"{len(lines)} listed as slower")
    return 1 if problems else 0


def list_on_main():
    """slower.txt as main has it, or None when there is no origin/main. A main without the file
    lists nothing, which only the commit that adds the file may differ from."""
    relative = SLOWER.relative_to(root).as_posix()
    known = subprocess.run(["git", "rev-parse", "--verify", "-q", "origin/main"], cwd=root, capture_output=True, text=True)
    if known.returncode != 0:
        return None
    shown = subprocess.run(["git", "show", f"origin/main:{relative}"], cwd=root, capture_output=True, text=True)
    if shown.returncode != 0:
        return None
    return [line.strip() for line in shown.stdout.splitlines() if line.strip() and not line.startswith("#")]


def selected_ids(listing, pattern, ci):
    ids = [case["id"] for case in listing["cases"]]
    if pattern:
        ids = [case for case in ids if re.search(pattern, case)]
    if ci:
        ids = [case for case in ids if not LARGE.search(case.rsplit("/", 1)[1])]
    return ids


def command_compare(args):
    listing = list_cases(args.rscript)
    ids = selected_ids(listing, args.filter, args.ci)
    table = engines(args.ry, args.rscript, jit_off=args.jit_off)
    measured = measure(ids, table, args.rounds, args.batch, args.batches, parallel=args.parallel)
    ratios, broken = case_ratios(measured, ids)
    weights = call_weights(listing)
    rows = []
    for case, by_engine in ratios.items():
        gnu = statistics.median(round_[case]["seconds"] for round_ in measured["gnu_r"])
        worst = min((ratio if ratio is not None else 0.0) for ratio in by_engine.values())
        rows.append((worst, case, gnu, by_engine))
    rows.sort()
    engine_names = [engine for engine in table if engine != "gnu_r"]
    print("| case | GNU R per call | " + " | ".join(engine_names) + " | weight |")
    print("|---|---:|" + "---:|" * len(engine_names) + "---:|")
    for worst, case, gnu, by_engine in rows:
        cells = []
        for engine in engine_names:
            ratio = by_engine.get(engine)
            if ratio is None:
                run = next((round_[case] for round_ in measured[engine] if round_.get(case, {}).get("seconds") is None), {})
                cells.append(f"fails: {run.get('reason', '')[:60]}")
            else:
                cells.append(f"{ratio:.2f}x")
        print(f"| {case} | {per_call(gnu)} | " + " | ".join(cells) + f" | {weights.get(function_of(case), 0):.4f} |")
    for case, reason in broken.items():
        print(f"broken case (fails on GNU R): {case}: {reason}")
    if args.save:
        import json
        pathlib.Path(args.save).write_text(json.dumps({"measured": measured, "ratios": ratios}, indent=1))
    return 0


def per_call(seconds):
    for unit, scale in (("s", 1), ("ms", 1e3), ("us", 1e6)):
        if seconds * scale >= 1:
            return f"{seconds * scale:.3g} {unit}"
    return f"{seconds * 1e9:.3g} ns"


def command_ratchet(args):
    listing = list_cases(args.rscript)
    if listing["missing"] or listing["unknown"] or listing["broken"]:
        sys.exit("error: the sweep does not cover every export; run `sweep.py list`")
    ids = selected_ids(listing, args.filter, args.ci)
    table = engines(args.ry, args.rscript, jit_off=not args.ci or args.jit_off)
    batch, batches = (0.004, 3) if args.ci else (args.batch, args.batches)
    measured = measure(ids, table, args.rounds, batch, batches)
    ratios, broken = case_ratios(measured, ids)
    if broken:
        for case, reason in broken.items():
            print(f"error: {case} fails on GNU R: {reason}")
        sys.exit("error: a case is broken; fix it in cases/")
    listed = set(read_list())
    floor = 1 / (1 + args.tolerance)
    worst = function_ratios(ratios)
    suspects = [case for case, by_engine in ratios.items()
                if function_of(case) not in listed and any(r is None or r < floor for r in by_engine.values())]
    if suspects and args.confirm:
        # A short batch on a shared machine can lose a few tens of percent to a neighbour, so a
        # function that looks slower is timed again before it fails the ratchet.
        print(f"{len(suspects)} cases of unlisted functions look slower; timing them {args.confirm} more rounds",
              file=sys.stderr)
        again = measure(suspects, table, args.confirm, batch, batches)
        for engine in measured:
            measured[engine].extend({case: round_[case] for case in suspects if case in round_} for round_ in again[engine])
        ratios.update(case_ratios({engine: [round_ for round_ in rounds if all(case in round_ for case in suspects)]
                                   for engine, rounds in measured.items()}, suspects)[0])
        worst = function_ratios(ratios)
    failing, improved = [], []
    for name, (ratio, case, engine) in sorted(worst.items()):
        slower = ratio is None or ratio < 1
        if name in listed and not slower:
            improved.append(name)
        elif name not in listed and (ratio is None or ratio < floor):
            failing.append((name, ratio, case, engine))
    if args.seed:
        slower = sorted(name for name, (ratio, _, _) in worst.items()
                        if ratio is None or ratio < floor or (name in listed and ratio < 1))
        write_list(slower)
        print(f"wrote {len(slower)} functions to {SLOWER.relative_to(root)}")
        return 0
    for name, ratio, case, engine in failing:
        what = "fails" if ratio is None else f"runs at {ratio:.2f}x"
        print(f"SLOWER: {name} {what} on {engine} in {case}, and slower.txt does not list it")
    measured_functions = {function_of(case) for case in ids}
    if improved:
        print(f"{len(improved)} listed functions are no longer slower: {', '.join(improved)}")
        if args.update:
            write_list(sorted(listed - set(improved)))
            print(f"removed them from {SLOWER.relative_to(root)}")
        else:
            print("run with --update to remove them from slower.txt")
    print(f"{len(measured_functions)} functions timed, {len([n for n in listed if n in measured_functions])} listed as slower, "
          f"{len(failing)} newly slower")
    return 1 if failing else 0


def command_validate(args):
    listing = list_cases(args.rscript)
    ids = selected_ids(listing, args.filter, args.ci)
    table = engines(args.ry, args.rscript, jit_off=False)
    if not args.with_ry:
        del table["ry"]
    measured = measure(ids, table, 1, 0.002, 1)
    gnu = measured["gnu_r"][0]
    problems = 0
    for case in ids:
        run = gnu.get(case, {"seconds": None, "reason": "not run"})
        if run["seconds"] is None:
            problems += 1
            print(f"FAILS ON GNU R: {case}: {run['reason']}")
        elif run["seconds"] > 1:
            problems += 1
            print(f"TOO SLOW ON GNU R: {case}: {run['seconds']:.2f} s per call; use a smaller input")
    if "ry" in measured:
        ry = measured["ry"][0]
        for case in ids:
            run, reference = ry.get(case, {"seconds": None, "reason": "not run"}), gnu.get(case, {})
            if run["seconds"] is None:
                print(f"fails on ry: {case}: {run['reason']}")
            elif reference.get("seconds") is not None and run["checksum"] != reference["checksum"]:
                print(f"differs on ry: {case}: {run['checksum']} against GNU R's {reference['checksum']}")
    print(f"{len(ids)} cases, {problems} to fix")
    return 1 if problems else 0


def call_weights(listing):
    """Each exported function's share of the calls in the code-base templates and in the oracle's
    cases, averaged over the two corpora, by a count of `name(` in the sources."""
    corpora = [sorted((root / "tools/bench/tooling/templates").rglob("*.R")),
               sorted((root / "crates/oracle/cases").glob("*.R"))]
    names = ({function_of(case["id"]) for case in listing["cases"]} | set(listing["missing"])
             | {f"{skip['package']}::{skip['function']}" for skip in listing["skips"]})
    plain = collections.defaultdict(list)
    for qualified in names:
        plain[qualified.split("::", 1)[1]].append(qualified)
    pattern = re.compile(r"(?<![\w.])([A-Za-z.][\w.]*)\s*\(")
    weights = collections.Counter()
    for files in corpora:
        counts = collections.Counter()
        for path in files:
            counts.update(match for match in pattern.findall(path.read_text(errors="replace")) if match in plain)
        total = sum(counts.values()) or 1
        for name, count in counts.items():
            for qualified in plain[name]:
                weights[qualified] += count / total / len(corpora)
    return weights


def command_weights(args):
    for name, weight in call_weights(list_cases(args.rscript)).most_common(args.top):
        print(f"{weight:.5f}\t{name}")
    return 0


def main():
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("command", choices=["list", "check", "compare", "ratchet", "validate", "weights"])
    parser.add_argument("--rscript", default=os.environ.get("ORACLE_R_RSCRIPT", shutil.which("Rscript") or "Rscript"))
    parser.add_argument("--ry", default=str(root / "target/release/ry"))
    parser.add_argument("--filter", default="", help="a regular expression the case ids must match")
    parser.add_argument("--rounds", type=int, default=1)
    parser.add_argument("--batch", type=float, default=0.01, help="seconds per timed batch")
    parser.add_argument("--batches", type=int, default=5)
    parser.add_argument("--jit-off", action="store_true", help="also time ry with RY_JIT=off")
    parser.add_argument("--ci", action="store_true", help="the cases without a 1e6 input, in short batches")
    parser.add_argument("--update", action="store_true", help="ratchet: remove functions that are no longer slower")
    parser.add_argument("--seed", action="store_true",
                        help="ratchet: rewrite slower.txt as every function that is slower now")
    parser.add_argument("--tolerance", type=float, default=0.1,
                        help="ratchet: how much slower than GNU R an unlisted function may measure before it fails")
    parser.add_argument("--confirm", type=int, default=2, help="ratchet: more rounds for functions that look slower")
    parser.add_argument("--with-ry", action="store_true", help="validate: also run the cases on ry")
    parser.add_argument("--parallel", action="store_true",
                        help="compare: run the engines side by side on separate cores (faster, noisier)")
    parser.add_argument("--save", help="compare: keep the raw measurements as JSON")
    parser.add_argument("--top", type=int, default=100, help="weights: how many functions to print")
    args = parser.parse_args()
    if args.ci and args.command == "ratchet" and args.tolerance == 0.1:
        args.tolerance = 0.25
    commands = {"list": command_list, "check": command_check, "compare": command_compare,
                "ratchet": command_ratchet, "validate": command_validate, "weights": command_weights}
    sys.exit(commands[args.command](args))


if __name__ == "__main__":
    main()
