#!/usr/bin/env python3
"""Runs ry's published benchmark suites on GNU R and on ry and writes each suite's results.json.

The four suites, each described by the suite.toml in its directory:

  public        Public R JIT benchmarks: the 54 Ř and rcp programs, vendored under public/.
  packages      Package test suites: the pinned CRAN packages of packages/lock.tsv.
  kernels       Numeric kernels: our loops, recursion, arrays and numeric math.
  applications  Application workloads: our calls, dispatch, conditions, records and tables.

The engines are GNU R, ry, and ry typed: a program with a `<name>.typed.R` beside it also runs
that typed version in strict mode. Every run is a fresh process. A round runs every program on
every engine once, programs in an order shuffled per round and engines in an order shuffled per
program, so no engine always follows another and inherits its cache state, and a drift of the
machine's speed hits all engines alike. Each suite first evicts every ry build's files from the
page cache (evict.py), so peak RSS compares builds from the same page-cache state. A program's time on an engine is the median over rounds,
and its ratio to GNU R is the median of the per-round ratios, with a 95% bootstrap interval over
rounds. A suite's geometric mean has a bootstrap interval that resamples each program's rounds.

A run fails, and writes nothing, when any program fails on any engine or prints a checksum that
differs from GNU R's (a numeric checksum may differ by a relative 1e-10, the room a different
summation order needs). The packages suite is the exception, because there a package's own test
suite is the program: test outcomes that differ between engines are recorded as a `tests_differ`
flag (a test script that ends in an R error counts as one failed test), and a test suite that
crashes or does not run to the end on an engine is recorded as a failure in that package's row, which leaves the package out of the geometric mean. Flags mark a program without failing it: `too_short` when an engine's median is
under 0.1 s (a program whose GNU R time is that short is also left out of the geometric mean),
`wide_interval` when the interval of a ratio reaches more than 10% from its median (such programs
get `--extra-rounds` more rounds first), and `typed_refused` when the typed core refused part of a
typed program, with the refusal that `RY_TYPED_LOG` printed.

Before anything runs, every ry under test must load only start-up and code images its own runtime
build wrote, and two binaries must not share a base R tree; otherwise the run stops and names the
stale files (`refuse_foreign_images`).

Every suite checks before it runs that the corpus library holds each package its programs load, at
the version lock.tsv pins (for the packages suite, every locked package), and stops with the list of
missing ones. `--partial` runs anyway: the summary and results.json say the run is partial and name
the missing packages, and `--write` refuses such a run.

`--samples FILE` keeps every round's raw samples in FILE as it goes, and a later run with the same
FILE and options resumes after the last complete round. `--write` needs at least 10 rounds and
every program, and stores results.json; tools/bench/readme.py renders the README from it.

Usage: tools/bench/bench.py SUITE... | all [--ry PATH] [--rscript PATH] [--rounds N] [--seed N]
                            [--filter TEXT] [--extra-rounds N] [--samples FILE] [--write]
                            [--baseline PATH] [--jit-off] [--partial]
"""
import argparse, json, math, os, pathlib, platform, random, shlex, statistics, subprocess, sys
import re, tempfile, threading, time, tomllib

here = pathlib.Path(__file__).resolve().parent
root = here.parent.parent
sys.path.insert(0, str(here / "tooling"))
sys.path.insert(0, str(here))
import packages as corpus  # noqa: E402
from evict import evict  # noqa: E402

SUITES = ["public", "packages", "kernels", "applications"]
# The suites whose programs may have a typed version, which the `ry typed` engine runs.
TYPED_SUITES = ("public", "kernels", "applications")
SCHEMA = 1
# The fewest rounds a published run may have: fewer cannot separate a 10% change from noise on a
# shared cloud machine, where one fixed C loop varies by about 25% between processes.
PUBLISHED_ROUNDS = 10
# Below this many seconds a time mostly measures start-up and the timer.
SHORT = 0.1
# An interval reaching further than this from its median is too wide to quote.
WIDE = 0.10
PROGRAM_TIMEOUT = 300
STRICT_CONFIG = "[check]\ntyping = true\nstrict = true\n"


def main():
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("suites", nargs="+", metavar="SUITE", help=f"{', '.join(SUITES)} or all")
    parser.add_argument("--ry", default=str(root / "target/release/ry"))
    parser.add_argument("--rscript", default=os.environ.get("ORACLE_R_RSCRIPT", "Rscript"),
                        help="GNU R's Rscript")
    parser.add_argument("--rounds", type=int, default=PUBLISHED_ROUNDS)
    parser.add_argument("--seed", type=int, default=1)
    parser.add_argument("--filter", default="", help="only programs whose id contains this, or whose group is this")
    parser.add_argument("--extra-rounds", type=int, default=10,
                        help="more rounds for each program with a wide interval")
    parser.add_argument("--samples", help="keep raw samples here and resume from them")
    parser.add_argument("--write", action="store_true", help="store each suite's results.json")
    parser.add_argument("--baseline", metavar="PATH", help="a second ry, timed in the same rounds")
    parser.add_argument("--jit-off", action="store_true", help="also time ry with RY_JIT=off")
    parser.add_argument("--partial", action="store_true",
                        help="run even when the corpus library lacks packages, and mark the results partial")
    args = parser.parse_args()
    suites = SUITES if "all" in args.suites else args.suites
    for suite in suites:
        if suite not in SUITES:
            sys.exit(f"error: no suite {suite!r}; the suites are {', '.join(SUITES)}")
    if args.write:
        if args.rounds < PUBLISHED_ROUNDS:
            sys.exit(f"error: --write needs at least {PUBLISHED_ROUNDS} rounds")
        if args.filter or args.baseline or args.jit_off:
            sys.exit("error: --write records every program on the published engines only")
        if args.partial:
            sys.exit("error: --write records complete runs only, so it does not take --partial")
        changes = sorted({line for tree in {checkout_of(args.ry), str(root)} for line in changed_paths(tree)})
        if changes:
            sys.exit("error: --write records a clean commit, but the working tree has changes; commit or "
                     "remove them and rebuild ry:\n" + "\n".join(f"  {line}" for line in changes))
    for binary in filter(None, [args.ry, args.baseline]):
        refuse_unreleased(binary)
    refuse_foreign_images(list(filter(None, [args.ry, args.baseline])))
    manifests = {suite: load_suite(suite) for suite in suites}
    missing = {suite: missing_packages(suite, selected(manifest, args)) for suite, manifest in manifests.items()}
    if any(missing.values()) and not args.partial:
        sys.exit("error: the corpus library lacks packages the suites load; run "
                 "`just bench-packages install`, or pass --partial to run without them:\n"
                 + "\n".join(f"  {suite}: {', '.join(names)}" for suite, names in missing.items() if names))
    meta = metadata(args)
    print(f"Machine: {meta['machine']['cpu']}, {meta['machine']['cores']} cores, {meta['machine']['kernel']}")
    print(f"GNU R: {meta['gnu_r']['version']}, BLAS {meta['gnu_r']['blas']}; ry {meta['ry_commit']}\n")
    failed = False
    for suite in suites:
        failed |= run_suite(suite, manifests[suite], missing[suite], args, meta)
    if failed:
        sys.exit("error: a program failed or disagreed with GNU R; nothing was written")


# Suites and engines.

class Engine:
    def __init__(self, key, label, command, env, typed=False):
        self.key, self.label, self.command, self.env, self.typed = key, label, command, env, typed


def engines(args, suite):
    base = {key: value for key, value in os.environ.items() if key not in ("RY_JIT", "RY_TYPED_LOG")}
    base.update(LC_ALL="C.UTF-8")
    library = corpus.corpus / "library"
    if library.is_dir():
        # Both engines load packages from the corpus library and then base R's own, so neither
        # reads a different build of a package from a site library.
        gnu_r_library = subprocess.run([args.rscript, "--vanilla", "-e", "cat(.Library)"],
                                       capture_output=True, text=True, check=True).stdout.strip()
        base.update(R_LIBS=str(library), R_LIBS_USER=str(library), R_LIBS_SITE=gnu_r_library)
    ry = [args.ry, "run", "--runtime", "ry"]
    table = [Engine("gnu_r", "GNU R", [args.rscript, "--vanilla"], base),
             Engine("ry", "ry", ry, base)]
    typed = suite in TYPED_SUITES
    if typed:
        table.append(Engine("ry_typed", "ry typed", ry, base, typed=True))
    if args.jit_off:
        table.append(Engine("ry_nojit", "ry, JIT off", ry, dict(base, RY_JIT="off")))
    if args.baseline:
        baseline = [args.baseline, "run", "--runtime", "ry"]
        table.append(Engine("baseline", "baseline", baseline, base))
        if typed:
            table.append(Engine("baseline_typed", "baseline typed", baseline, base, typed=True))
    return table


def load_suite(suite):
    manifest = tomllib.loads((here / suite / "suite.toml").read_text())
    for program in manifest["programs"]:
        source = root / program["source"] if "source" in program else None
        if source is not None and not source.exists():
            sys.exit(f"error: {suite}/suite.toml names {program['source']}, which does not exist")
        if suite in TYPED_SUITES:
            typed = source.with_name(f"{program['id']}.typed.R")
            if typed.exists() != ("typed_source" in program):
                sys.exit(f"error: {suite}/suite.toml and the files disagree on whether {program['id']} has a typed version")
    listed = {program["id"] for program in manifest["programs"]}
    if suite in ("kernels", "applications"):
        present = {path.name.removesuffix(".R") for path in (here / suite).glob("*.R")
                   if not path.name.endswith(".typed.R")}
        if present != listed:
            sys.exit(f"error: {suite}/suite.toml lists {sorted(listed - present)} without a file and "
                     f"misses {sorted(present - listed)}")
    return manifest


def selected(manifest, args):
    return [program for program in manifest["programs"]
            if args.filter in program["id"] or args.filter == program.get("group")]


def missing_packages(suite, programs):
    """The packages the programs load that the corpus library lacks at the locked version, each
    with the reason. A package's test suite may load any locked package, so the packages suite
    needs all of them."""
    versions = {name: version for name, version, _ in corpus.locked()}
    if suite == "packages":
        needed = set(versions)
    else:
        needed = set()
        for program in programs:
            for key in ("source", "typed_source"):
                if key in program:
                    needed |= loaded_packages((root / program[key]).read_text())
        needed -= corpus.BASE_PACKAGES
    missing = []
    for name in sorted(needed, key=str.lower):
        if name not in versions:
            missing.append(f"{name} (not in packages/lock.tsv)")
            continue
        description = corpus.corpus / "library" / name / "DESCRIPTION"
        if not description.is_file():
            missing.append(name)
            continue
        installed = corpus.parse_dcf(description.read_text(errors="replace")).get("Version")
        if installed != versions[name]:
            missing.append(f"{name} ({installed} installed, {versions[name]} locked)")
    return missing


def loaded_packages(source):
    """The packages an R source attaches, loads or reaches into with `::`."""
    calls = re.findall(r"\b(?:library|require|requireNamespace|loadNamespace)\(\s*[\"']?([A-Za-z][A-Za-z0-9.]*)", source)
    return set(calls) | set(re.findall(r"\b([A-Za-z][A-Za-z0-9.]*):::?[A-Za-z.`]", source))


def run_suite(suite, manifest, missing, args, meta):
    """Times one suite and, with --write, stores its results. True when the run failed."""
    if missing:
        meta = dict(meta, partial={"missing_packages": missing})
    table = engines(args, suite)
    programs = selected(manifest, args)
    runner = {"public": PublicRunner, "packages": PackageRunner}.get(suite, WorkloadRunner)(suite, args)
    samples_path = pathlib.Path(args.samples).with_suffix(f".{suite}.json") if args.samples else None
    state = load_samples(samples_path, args, table, programs)
    shuffle = random.Random(f"{args.seed}:{suite}")
    for _ in range(len(state["rounds"])):
        shuffle_round(shuffle, programs, table)
    print(f"## {manifest['name']}: {len(programs)} programs, {args.rounds} rounds\n")
    if missing:
        print(f"PARTIAL RUN: the corpus library lacks {', '.join(missing)}\n", flush=True)
    # Peak RSS depends on how the page cache holds each build's files, which a fresh build holds
    # differently from one read back from disk (see evict.py), so every ry build starts evicted.
    evict([args.ry] + ([args.baseline] if args.baseline else []))
    try:
        while len(state["rounds"]) < args.rounds:
            number = len(state["rounds"]) + 1
            started = time.monotonic()
            state["rounds"].append(run_round(runner, shuffle_round(shuffle, programs, table), state))
            save_samples(samples_path, state)
            print(f"round {number} of {args.rounds} took {time.monotonic() - started:.0f} s", flush=True)
        wide = [program for program in programs
                if program["id"] not in state["extra"] and wide_interval(state, program["id"], table)]
        if wide and args.extra_rounds:
            names = ", ".join(program["id"] for program in wide)
            print(f"{len(wide)} programs have an interval wider than ±{WIDE:.0%}; "
                  f"{args.extra_rounds} more rounds for {names}", flush=True)
            for _ in range(args.extra_rounds):
                state["rounds"].append(run_round(runner, shuffle_round(shuffle, wide, table), state))
                save_samples(samples_path, state)
            state["extra"] += [program["id"] for program in wide]
            save_samples(samples_path, state)
        results = summarize(suite, manifest, programs, table, state, args, meta, runner)
    finally:
        runner.close()
    print_table(results, table)
    if missing:
        print(f"\nPARTIAL RUN: the corpus library lacks {', '.join(missing)}, so programs that load "
              "them failed or were measured without them")
    failures = [(program, failure) for program in results["programs"] for failure in program.get("failures", [])]
    for program, failure in failures:
        print(f"FAILED {suite}/{program['id']} on {failure['engine']}: {failure['reason']}")
    if runner.network:
        print("the tests reached for the network, which skews both engines' times; add each test file "
              "to NETWORK_TESTS in tooling/packages.py:\n  " + "\n  ".join(sorted(set(runner.network))))
    # A package's own test suite failing is a compatibility result, recorded in its row like
    # tests_differ. One of our programs failing means the measurement itself is broken.
    blocking = [] if suite == "packages" else failures
    if args.write and not blocking and not runner.network:
        (here / suite / "results.json").write_text(json.dumps(results, indent=2, ensure_ascii=False) + "\n")
        print(f"wrote {here / suite / 'results.json'}")
    print()
    return bool(blocking or runner.network)


def shuffle_round(shuffle, programs, table):
    order = list(programs)
    shuffle.shuffle(order)
    plan = []
    for program in order:
        engines_here = [engine for engine in table if not engine.typed or "typed_source" in program]
        shuffle.shuffle(engines_here)
        plan.append((program, engines_here))
    return plan


def run_round(runner, plan, state):
    measured = {}
    for program, engines_here in plan:
        results = {}
        for engine in engines_here:
            results[engine.key] = runner.run(program, engine)
        measured[program["id"]] = results
        line = " ".join(f"{key} {describe(result)}" for key, result in sorted(results.items()))
        print(f"  {program['id']}: {line}", flush=True)
    return measured


def describe(result):
    if result.get("seconds") is None:
        return f"fails ({result.get('reason', '')[:80]})"
    return f"{result['seconds']:.3f}s"


# Running one program on one engine.

def spawn(command, env, cwd=None, timeout=PROGRAM_TIMEOUT):
    """Runs a process to the end and returns its exit status, output, wall seconds and peak
    resident set in MiB. `wait4` reports the child's own peak, which covers the whole process from
    start-up on and any process it waited for."""
    with tempfile.TemporaryFile("w+") as stdout, tempfile.TemporaryFile("w+") as stderr:
        started = time.monotonic()
        child = subprocess.Popen(command, stdout=stdout, stderr=stderr, text=True, env=env, cwd=cwd)
        expired = threading.Event()

        def expire():
            expired.set()
            child.kill()

        timer = threading.Timer(timeout, expire)
        timer.start()
        try:
            _, status, usage = os.wait4(child.pid, 0)
        finally:
            timer.cancel()
        child.returncode = os.waitstatus_to_exitcode(status)
        seconds = time.monotonic() - started
        stdout.seek(0)
        stderr.seek(0)
        out, err = stdout.read(), stderr.read()
    if expired.is_set():
        return None, out, f"timed out after {timeout} s", seconds, usage.ru_maxrss / 1024
    return child.returncode, out, err, seconds, usage.ru_maxrss / 1024


def same_checksum(value, reference):
    """Numeric checksums agree to a relative 1e-10, since a typed program may sum in another order;
    any other checksum must match exactly."""
    try:
        number, expected = float(value), float(reference)
    except ValueError:
        return value == reference
    return value == reference or math.isclose(number, expected, rel_tol=1e-10, abs_tol=1e-300)


def failure_reason(out, err):
    return ((err.strip() or out.strip() or "failed").splitlines() or ["failed"])[-1][:300]


class WorkloadRunner:
    """Kernels and applications: the program after harness.R (or harness.typed.R, in strict mode),
    once cold and five times warm in one process, which reports the median warm time."""

    def __init__(self, suite, args):
        self.suite = suite
        self.network = []
        self.work = pathlib.Path(tempfile.mkdtemp(prefix=f"ry-bench-{suite}-"))
        (self.work / "plain").mkdir()
        (self.work / "typed").mkdir()
        (self.work / "typed" / "ry.toml").write_text(STRICT_CONFIG)
        self.harness = (here / "harness.R").read_text()
        self.typed_harness = (here / "harness.typed.R").read_text()
        self.refusals = {}

    def run(self, program, engine):
        if engine.typed:
            source, harness, folder = root / program["typed_source"], self.typed_harness, self.work / "typed"
        else:
            source, harness, folder = root / program["source"], self.harness, self.work / "plain"
        script = folder / f"{program['id']}.R"
        script.write_text(harness + "\n" + source.read_text())
        status, out, err, _, peak = spawn(engine.command + [script.name], engine.env, cwd=folder)
        lines = [line for line in out.splitlines() if line.startswith(program["id"] + " ")]
        if status != 0 or not lines:
            return {"seconds": None, "reason": failure_reason(out, err)}
        _, checksum, median, _, cold = lines[-1].split()
        return {"seconds": float(median), "checksum": checksum, "cold": float(cold), "peak_mib": round(peak, 1)}

    def typed_refusal(self, program, engine):
        """The refusals `RY_TYPED_LOG` reports for the typed program beyond those of the harness
        alone, or None. Measured once, outside the timed rounds, since logging costs time."""
        def log(text):
            script = self.work / "typed" / "refusals.R"
            script.write_text(self.typed_harness + "\n" + text)
            env = dict(engine.env, RY_TYPED_LOG="1", RY_BENCH_REPEAT="1")
            status, out, err, _, _ = spawn(engine.command + [script.name], env, cwd=self.work / "typed")
            if status != 0:
                sys.exit(f"error: the typed refusal log run failed: {failure_reason(out, err)}")
            return [line for line in err.splitlines() if line.startswith("typed: ") and "compiled for" not in line]
        if "" not in self.refusals:
            self.refusals[""] = log('measure("empty", function() Num.to_f64(0))\n')
        lines = log((root / program["typed_source"]).read_text())
        for line in self.refusals[""]:
            if line in lines:
                lines.remove(line)
        return "; ".join(dict.fromkeys(line.removeprefix("typed: ") for line in lines)) or None

    def close(self):
        for path in sorted(self.work.rglob("*"), reverse=True):
            path.rmdir() if path.is_dir() else path.unlink()
        self.work.rmdir()


class PublicRunner:
    """Public programs: one run per process from the program's own directory, which prints
    `<name> <checksum> <seconds>`; the time includes byte compilation and tier-up. The typed
    version runs from a copy in a strict-mode project, still from the program's own directory,
    because some programs read sibling files by relative path."""

    def __init__(self, suite, args):
        self.network = []
        self.work = pathlib.Path(tempfile.mkdtemp(prefix="ry-bench-public-"))
        (self.work / "ry.toml").write_text(STRICT_CONFIG)

    def run(self, program, engine):
        source = root / program["source"]
        script = source.name
        if engine.typed:
            script = self.work / f"{program['id']}.R"
            script.write_text((root / program["typed_source"]).read_text())
        status, out, err, _, peak = spawn(engine.command + [str(script)], engine.env, cwd=source.parent)
        stem = source.name.removesuffix(".R")
        lines = [line for line in out.splitlines() if line.startswith(stem + " ")]
        if status != 0 or not lines:
            return {"seconds": None, "reason": failure_reason(out, err)}
        _, checksum, seconds = lines[-1].split()
        return {"seconds": float(seconds), "checksum": checksum, "peak_mib": round(peak, 1)}

    def typed_refusal(self, program, engine):
        """The refusals `RY_TYPED_LOG` reports for the typed program, or None. Measured once,
        outside the timed rounds, since logging costs time."""
        source = root / program["source"]
        script = self.work / f"{program['id']}.R"
        script.write_text((root / program["typed_source"]).read_text())
        status, out, err, _, _ = spawn(engine.command + [str(script)], dict(engine.env, RY_TYPED_LOG="1"),
                                       cwd=source.parent)
        if status != 0:
            sys.exit(f"error: the typed refusal log run failed: {failure_reason(out, err)}")
        lines = [line.removeprefix("typed: ") for line in err.splitlines()
                 if line.startswith("typed: ") and "compiled for" not in line]
        return "; ".join(dict.fromkeys(lines)) or None

    def close(self):
        for path in sorted(self.work.rglob("*"), reverse=True):
            path.rmdir() if path.is_dir() else path.unlink()
        self.work.rmdir()


class PackageRunner:
    """Package test suites: the wall time of a whole process that runs the package's tests, with
    every HTTP request refused by a local proxy that records the host."""

    def __init__(self, suite, args):
        library = corpus.corpus / "library"
        if not library.is_dir():
            sys.exit("error: no corpus library; run `just bench-packages install` first")
        self.versions = {name: version for name, version, _ in corpus.locked()}
        self.proxy = corpus.RefusingProxy()
        self.network = []
        self.work = pathlib.Path(tempfile.mkdtemp(prefix="ry-bench-packages-"))
        self.unpacked = {}

    def run(self, program, engine):
        name = program["id"]
        if name not in self.unpacked:
            unpacked = corpus.unpack_tests(name, self.versions[name], self.work)
            if unpacked is None:
                sys.exit(f"error: {name} ships no tests; drop it from packages/suite.toml")
            self.unpacked[name] = unpacked
        kind, tests = self.unpacked[name]
        env = dict(engine.env, TESTTHAT_CPUS="1", _R_CHECK_FORCE_SUGGESTS_="false")
        env.pop("NOT_CRAN", None)
        for variable in ("http_proxy", "https_proxy", "HTTPS_PROXY", "ALL_PROXY", "all_proxy"):
            env[variable] = self.proxy.url
        for variable in ("no_proxy", "NO_PROXY"):
            env[variable] = corpus.LOCAL_HOSTS
        result = self.run_tests(engine, env, name, kind, tests)
        self.network += [f"{name} on {engine.label}: {host}" for host in self.proxy.take()]
        return result

    def run_tests(self, engine, env, name, kind, tests):
        if kind == "scripts":
            # A test script reports failing tests the way `R CMD check` reads them, by ending in an
            # R error, which exits with status 1 (`test.data.table()` raises its error summary
            # after running every test). Such a script ran to its end and counts as failed. A
            # signal, a timeout or any other status means the suite did not finish.
            seconds, peak, passed, failed = 0.0, 0.0, 0, 0
            for script in sorted(tests.glob("*.R")):
                status, out, err, elapsed, script_peak = spawn(engine.command + [script.name], env, cwd=tests,
                                                               timeout=corpus.SUITE_TIMEOUT)
                if status not in (0, 1):
                    return {"seconds": None, "reason": f"{script.name}: {failure_reason(out, err)}"}
                seconds += elapsed
                peak = max(peak, script_peak)
                passed += status == 0
                failed += status == 1
            return {"seconds": round(seconds, 3), "checksum": f"{passed} passed, {failed} failed, 0 errors",
                    "tests": {"passed": passed, "failed": failed, "errors": 0}, "peak_mib": round(peak, 1)}
        driver = self.work / "driver.R"
        driver.write_text(corpus.driver(name, kind, tests))
        status, out, err, seconds, peak = spawn(engine.command + [str(driver)], env, cwd=tests,
                                                timeout=corpus.SUITE_TIMEOUT)
        lines = [line for line in out.splitlines() if line.startswith("RESULT ")]
        if status != 0 or not lines:
            return {"seconds": None, "reason": failure_reason(out, err)}
        passed, failed, _, errors = (int(value) for value in lines[-1].split()[1:])
        return {"seconds": round(seconds, 3), "checksum": f"{passed} passed, {failed} failed, {errors} errors",
                "tests": {"passed": passed, "failed": failed, "errors": errors}, "peak_mib": round(peak, 1)}

    def close(self):
        subprocess.run(["rm", "-rf", str(self.work)], check=True)


# Samples.

def load_samples(path, args, table, programs):
    key = {"rounds": args.rounds, "seed": args.seed, "filter": args.filter, "ry": args.ry,
           "engines": [engine.key for engine in table], "programs": [program["id"] for program in programs]}
    if path is not None and path.exists():
        state = json.loads(path.read_text())
        if state.get("key") == key:
            print(f"resuming after round {len(state['rounds'])} from {path}")
            return state
        sys.exit(f"error: {path} holds samples of a run with other options; remove it or pick another file")
    return {"key": key, "rounds": [], "extra": []}


def save_samples(path, state):
    if path is not None:
        partial = path.with_suffix(".partial")
        partial.write_text(json.dumps(state))
        partial.replace(path)


# Statistics.

def samples_of(state, program, key):
    return [round_[program][key] for round_ in state["rounds"]
            if program in round_ and key in round_[program] and round_[program][key].get("seconds") is not None]


def paired(state, program, numerator, denominator):
    """The ratio of two engines' times in each round where both ran. Pairing within a round cancels
    a drift in the machine's speed that lasts longer than one round."""
    ratios = []
    for round_ in state["rounds"]:
        results = round_.get(program, {})
        top, bottom = results.get(numerator, {}), results.get(denominator, {})
        if top.get("seconds") and bottom.get("seconds"):
            ratios.append(top["seconds"] / bottom["seconds"])
    return ratios


def interval(ratios, seed, resamples=2000):
    """The median of the per-round ratios and a 95% percentile bootstrap interval of it."""
    draw = random.Random(seed)
    medians = sorted(statistics.median(draw.choices(ratios, k=len(ratios))) for _ in range(resamples))
    return statistics.median(ratios), medians[int(0.025 * resamples)], medians[int(0.975 * resamples) - 1]


def wide_interval(state, program, table):
    for engine in table[1:]:
        ratios = paired(state, program, "gnu_r", engine.key)
        if len(ratios) >= 2:
            median, low, high = interval(ratios, 0)
            if high > median * (1 + WIDE) or low < median * (1 - WIDE):
                return True
    return False


def geomean_interval(per_program, seed, resamples=2000):
    """The geometric mean of each program's median paired ratio, and a 95% bootstrap interval
    that resamples each program's rounds."""
    point = geomean(statistics.median(ratios) for ratios in per_program)
    draw = random.Random(seed)
    means = sorted(geomean(statistics.median(draw.choices(ratios, k=len(ratios))) for ratios in per_program)
                   for _ in range(resamples))
    return point, means[int(0.025 * resamples)], means[int(0.975 * resamples) - 1]


def geomean(values):
    return math.exp(statistics.fmean(math.log(value) for value in values))


def summarize(suite, manifest, programs, table, state, args, meta, runner):
    rows = []
    included = {engine.key: [] for engine in table[1:]}
    for program in programs:
        name = program["id"]
        row = {"id": name, "display": program["display"], "seconds": {}, "ratio": {}, "peak_mib": {}, "flags": []}
        if suite == "packages":
            row["version"] = program["version"]
            row["runner"] = program["runner"]
            row["tests"] = {}
        failures = []
        engines_here = [engine for engine in table if not engine.typed or "typed_source" in program]
        reference = None
        for engine in engines_here:
            runs = [round_[name][engine.key] for round_ in state["rounds"] if engine.key in round_.get(name, {})]
            broken = [run for run in runs if run.get("seconds") is None]
            if broken:
                failures.append({"engine": engine.key, "reason": broken[0]["reason"]})
                continue
            if not runs:
                continue
            checksums = sorted({run["checksum"] for run in runs})
            if suite == "packages":
                row["tests"][engine.key] = runs[-1]["tests"]
            elif len(checksums) > 1:
                failures.append({"engine": engine.key, "reason": f"the checksum varies between runs: {', '.join(checksums)}"})
            if engine.key == "gnu_r":
                reference = checksums[0]
            elif reference is not None and not same_checksum(checksums[0], reference):
                if suite == "packages":
                    row["flags"].append({"kind": "tests_differ", "engine": engine.key,
                                         "reason": f"GNU R: {reference}; {engine.label}: {checksums[0]}"})
                else:
                    failures.append({"engine": engine.key,
                                     "reason": f"checksum {checksums[0]}, GNU R printed {reference}"})
            median = statistics.median(run["seconds"] for run in runs)
            row["seconds"][engine.key] = round(median, 4)
            row["peak_mib"][engine.key] = max(run["peak_mib"] for run in runs)
            if median < SHORT:
                row["flags"].append({"kind": "too_short", "engine": engine.key})
        if failures:
            row["failures"] = failures
        short = row["seconds"].get("gnu_r", SHORT) < SHORT
        for engine in engines_here[1:]:
            ratios = paired(state, name, "gnu_r", engine.key)
            if not ratios or failures:
                continue
            median, low, high = interval(ratios, args.seed)
            row["ratio"][engine.key] = {"median": round(median, 4), "low": round(low, 4), "high": round(high, 4)}
            if high > median * (1 + WIDE) or low < median * (1 - WIDE):
                row["flags"].append({"kind": "wide_interval", "engine": engine.key})
            if not short:
                included[engine.key].append(ratios)
        if suite in TYPED_SUITES and "typed_source" in program and not failures:
            typed = next(engine for engine in table if engine.typed)
            reason = runner.typed_refusal(program, typed)
            if reason:
                row["flags"].append({"kind": "typed_refused", "engine": typed.key, "reason": reason})
        rows.append(row)
    geomeans = {}
    for key, per_program in included.items():
        if per_program:
            point, low, high = geomean_interval(per_program, args.seed)
            geomeans[key] = {"median": round(point, 4), "low": round(low, 4), "high": round(high, 4),
                             "programs": len(per_program)}
    return {
        "schema": SCHEMA, "suite": suite, "name": manifest["name"],
        "meta": dict(meta, rounds=args.rounds, extra_rounds=args.extra_rounds, seed=args.seed),
        "engines": {engine.key: engine.label for engine in table},
        "programs": rows, "geomean": geomeans,
    }


def print_table(results, table):
    keys = [engine.key for engine in table if engine.key in results["engines"]]
    print("| program | " + " | ".join(results["engines"][key] for key in keys) + " | "
          + " | ".join(f"GNU R / {results['engines'][key]}" for key in keys[1:]) + " | flags |")
    print("| --- |" + " ---: |" * (2 * len(keys) - 1) + " --- |")
    for row in results["programs"]:
        cells = [f"{row['seconds'][key]:.3f} s" if key in row["seconds"] else "" for key in keys]
        for key in keys[1:]:
            ratio = row["ratio"].get(key)
            cells.append(f"{ratio['median']:.2f}x ({ratio['low']:.2f} to {ratio['high']:.2f})" if ratio else "")
        flags = ", ".join(f"{flag['kind']} ({flag['engine']})" for flag in row["flags"])
        if row.get("failures"):
            flags = ", ".join(["FAILED"] + ([flags] if flags else []))
        print(f"| {row['display']} | " + " | ".join(cells) + f" | {flags} |")
    for key, mean in results["geomean"].items():
        print(f"\nGeometric mean of GNU R / {results['engines'][key]} over {mean['programs']} programs: "
              f"{mean['median']:.2f}x (95% interval {mean['low']:.2f} to {mean['high']:.2f})")


# Metadata.

def refuse_unreleased(binary):
    """Exits unless `binary` is a ry built with `--release`.

    Any other profile names itself in `ry --version` (`ry 0.4.3-beta (iterate build)`): a debug build
    is many times slower, and the iterate profile skips the cross-crate inlining LTO does, so a timing
    of either would pass for a release number it is not.
    """
    try:
        version = subprocess.run([binary, "--version"], capture_output=True, text=True, timeout=60).stdout.strip()
    except (OSError, subprocess.TimeoutExpired) as error:
        sys.exit(f"error: cannot run {binary}: {error}")
    if re.search(r"\b[\w.-]+ build\b", version):
        sys.exit(f"error: {binary} is not a release build ({version}); benchmarks time release builds "
                 "only, so build it with `cargo build --release -p ry-lang -p libr`")


def refuse_foreign_images(binaries):
    """Exits when a ry under test would load a start-up or code image another build wrote, or when
    two of them share one base R tree.

    An image loads only into the runtime build that wrote it, and every other build ignores it, so
    the packages it covers run without their compiled code and start-up takes the slow path, and a
    comparison would credit that time to the code under test. Each binary runs once first, because a start rewrites a stale start-up image and the base
    packages' images; the recommended packages' images are rewritten only by `just base-r`, and a
    package library's only by reinstalling the package.
    """
    homes = {}
    for binary in binaries:
        identity = runtime_identity(binary)
        home = base_tree(binary)
        if identity is None or home is None:
            # A release embeds its runtime and base R, whose images it wrote itself.
            continue
        if home in homes:
            sys.exit(f"error: {homes[home]} and {binary} share the base R tree {home}, so each start "
                     "of one rewrites the start-up image the other loads; give each build its own "
                     "copy with `ry-rcmd start-image BASE --libr LIBR` and point RY_BASE_R at it")
        homes[home] = binary
        with tempfile.NamedTemporaryFile("w", suffix=".R") as script:
            script.write("invisible(NULL)\n")
            script.flush()
            subprocess.run([binary, "run", "--runtime", "ry", script.name], capture_output=True, timeout=600)
        libraries = [home / "library", corpus.corpus / "library"]
        libraries += [pathlib.Path(part) for variable in ("R_LIBS", "R_LIBS_USER")
                      for part in os.environ.get(variable, "").split(os.pathsep) if part]
        images = [home / "library/base/R/ry-start.image"]
        images += sorted({image for library in libraries if library.is_dir()
                          for image in library.glob("*/R/*.ryc")})
        stale = [image for image in images if image_writer(image) != identity]
        if stale:
            sys.exit(f"error: {binary} (runtime {identity}) would ignore these images, which another "
                     "build wrote, and run that code without them; run `just base-r` for base R's "
                     "packages and reinstall any other package with this build:\n"
                     + "\n".join(f"  {image} ({image_writer(image) or 'unreadable'})" for image in stale))


def runtime_identity(binary):
    """The identity of the runtime library beside `binary`, which stamps every image it writes, or
    None for a binary with no `libR` beside it."""
    directory = pathlib.Path(binary).resolve().parent
    library = next((directory / name for name in ("libR.so", "libR.dylib", "R.dll")
                    if (directory / name).is_file()), None)
    if library is None:
        return None
    import ctypes
    function = ctypes.CDLL(str(library), mode=os.RTLD_LOCAL).ry_runtime_id
    function.restype = ctypes.c_char_p
    return function().decode()


def base_tree(binary):
    """The base R tree `rcmd::base::installed` finds for `binary`, or None for none."""
    if os.environ.get("R_HOME"):
        return pathlib.Path(os.environ["R_HOME"]).resolve()
    if os.environ.get("RY_BASE_R"):
        return pathlib.Path(os.environ["RY_BASE_R"]).resolve()
    directory = pathlib.Path(binary).resolve().parent
    candidates = [directory / "base-r", directory / "../lib/ry/base-r"]
    target = next((ancestor for ancestor in [directory, *directory.parents]
                   if (ancestor / "CACHEDIR.TAG").is_file()), None)
    if target is not None and not directory.is_relative_to(target / "tmp"):
        for ancestor in [directory, *directory.parents]:
            candidates.append(ancestor / "base-r")
            if ancestor == target:
                break
    return next((candidate.resolve() for candidate in candidates
                 if (candidate / "library/base/R/base").is_file()), None)


def image_writer(path):
    """The runtime identity a start-up image (`RYSTART`) or code image (`RYCODE`) was written by,
    read from its header, or None when the file is neither."""
    try:
        with open(path, "rb") as file:
            header = file.read(4096)
    except OSError:
        return None
    # A start-up image puts its header's length before the header, which opens with the identity.
    offset = {b"RYSTART": 16, b"RYCODE\x00": 8}.get(header[:7])
    if offset is None or len(header) < offset + 8:
        return None
    length = int.from_bytes(header[offset:offset + 8], "little")
    return header[offset + 8:offset + 8 + length].decode(errors="replace")


def metadata(args):
    cpu = platform.processor() or platform.machine()
    try:
        for line in pathlib.Path("/proc/cpuinfo").read_text().splitlines():
            if line.startswith("model name"):
                cpu = line.split(":", 1)[1].strip()
                break
    except OSError:
        pass
    probe = subprocess.run([args.rscript, "--vanilla", "-e",
                            "cat(paste(R.version$major, R.version$minor, sep = '.'), '\\n', La_library(), '\\n', extSoftVersion()[['BLAS']], '\\n', sep = '')"],
                           capture_output=True, text=True, check=True).stdout.splitlines()
    version, lapack, blas = (probe + ["", "", ""])[:3]
    return {
        "date": time.strftime("%Y-%m-%d"),
        "ry_commit": commit(args.ry),
        "too_short_seconds": SHORT,
        "gnu_r": {"version": version.strip(), "blas": (blas or lapack).strip()},
        "machine": {"cpu": cpu, "cores": os.cpu_count(), "kernel": f"{platform.system()} {platform.release()}"},
        "command": "tools/bench/bench.py " + " ".join(shlex.quote(argument) for argument in sys.argv[1:]),
    }


def commit(ry):
    """The commit of the checkout that built the measured ry, marked `-dirty` when that working
    tree has the changes `changed_paths` lists. `--write` refuses those, so a stored commit is
    always clean."""
    checkout = checkout_of(ry)
    head = subprocess.run(["git", "-C", checkout, "rev-parse", "HEAD"],
                          capture_output=True, text=True, check=True).stdout.strip()
    return head + ("-dirty" if changed_paths(checkout) else "")


def checkout_of(ry):
    return subprocess.run(["git", "-C", str(pathlib.Path(ry).resolve().parent), "rev-parse", "--show-toplevel"],
                          capture_output=True, text=True, check=True).stdout.strip()


def changed_paths(checkout):
    """`git status` lines for what makes a measurement unreproducible from its commit: a change to
    any tracked file, and a file git does not track under crates/ (built into ry) or tools/bench/
    (the programs and the runner)."""
    tracked = subprocess.run(["git", "-C", checkout, "status", "--porcelain", "--untracked-files=no"],
                             capture_output=True, text=True, check=True).stdout.splitlines()
    untracked = subprocess.run(["git", "-C", checkout, "status", "--porcelain", "--untracked-files=all", "--",
                                "crates", "tools/bench"], capture_output=True, text=True, check=True).stdout.splitlines()
    return tracked + [line for line in untracked if line.startswith("??")]


if __name__ == "__main__":
    main()
