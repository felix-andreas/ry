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
With `--baseline`, every program and the suite also get the paired ratio baseline / ry the same
way, so a value above 1 means the build under test is faster.

Peak RSS comes in parts, because they behave differently (development/testing.md): the process's
whole peak (`VmHWM`), and the peaks of its anonymous memory outside the start-up image, of the
image's resident pages and of `libR.so`'s resident pages, all read from `/proc/<pid>` every 20 ms
while it runs (every engine alike; a process shorter than that has no sample). The run's
metadata holds the size of each ry build's `libR.so` `.text` section, which is judged in place of
its resident pages.

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

Every suite checks before it runs that the corpus library holds each package its programs load, at
the version lock.tsv pins (for the packages suite, every locked package), and stops with the list of
missing ones. `--partial` runs anyway: the summary and results.json say the run is partial and name
the missing packages, and `--write` refuses such a run.

A start-up image and a package's code image load only into the build of ry that wrote them, and a
build that cannot load them starts the slow way, which costs up to a sixth more instructions and
several MB more memory. Builds that share a base R tree each keep their own images there
(crates/rcmd/src/images.rs), and a build writes its images the first time it starts. Before the
first suite, bench.py starts each ry build once, so no timed process pays for writing them, and
stops when a package in the corpus library or `R_LIBS` has only another build's code image, naming
the packages to reinstall (`refuse_foreign_images`). Every ry process runs with `RY_IMAGE_LOG=1`,
and one that reports an unused image or writes images fails, so a run never compares a fast start
with a slow one. A build whose runtime predates `RY_IMAGE_LOG` cannot report one, so bench.py
refuses it.

`--samples FILE` keeps every round's raw samples in FILE as it goes, and a later run with the same
FILE and options resumes after the last complete round. A run of several suites keeps each suite's
in FILE with the suite's name before its extension (`samples.kernels.json`). `--write` needs at least 10 rounds and
every program, and stores results.json; tools/bench/readme.py renders the README from it.

Usage: tools/bench/bench.py SUITE... | all [--ry PATH] [--rscript PATH] [--rounds N] [--seed N]
                            [--filter TEXT] [--extra-rounds N] [--samples FILE] [--write]
                            [--baseline PATH] [--jit-off] [--partial]
"""
import argparse, json, math, os, pathlib, platform, random, shlex, statistics
import subprocess, sys, re, tempfile, threading, time, tomllib

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
# The lines ry prints on standard error under RY_IMAGE_LOG for each start-up or code image it could
# not use (crates/interp/src/image.rs, `report_unused`), and when it writes a build's images into a
# base R tree (crates/rcmd/src/rcmd.rs, `refresh_start_image`).
IMAGE_UNUSED = "ry: image unused: "
IMAGE_WRITTEN = "ry: image written: "
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
    # Programs run from their own directories.
    args.ry = os.path.abspath(args.ry)
    args.baseline = args.baseline and os.path.abspath(args.baseline)
    suites = SUITES if "all" in args.suites else args.suites
    args.suites = suites
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
    ry_env = dict(base, RY_IMAGE_LOG="1")
    table = [Engine("gnu_r", "GNU R", [args.rscript, "--vanilla"], base),
             Engine("ry", "ry", ry, ry_env)]
    typed = suite in TYPED_SUITES
    if typed:
        table.append(Engine("ry_typed", "ry typed", ry, ry_env, typed=True))
    if args.jit_off:
        table.append(Engine("ry_nojit", "ry, JIT off", ry, dict(ry_env, RY_JIT="off")))
    if args.baseline:
        baseline = [args.baseline, "run", "--runtime", "ry"]
        baseline_env = dict(base, RY_IMAGE_LOG="1")
        table.append(Engine("baseline", "baseline", baseline, baseline_env))
        if typed:
            table.append(Engine("baseline_typed", "baseline typed", baseline, baseline_env, typed=True))
    return table


def file_contains(path, needle):
    """Whether the file holds `needle`, read in 1 MiB pieces, so that scanning a 120 MiB libR does
    not hold all of it in memory."""
    tail = b""
    with open(path, "rb") as file:
        while piece := file.read(1 << 20):
            if needle in tail + piece:
                return True
            tail = piece[-(len(needle) - 1):]
    return False


def refuse_foreign_images(binaries):
    """Exits unless every ry build in `binaries` would load only images it wrote itself.

    An image loads only into the build whose identity its file name carries
    (crates/rcmd/src/images.rs), and any other build starts the slow way or runs the package without
    its compiled code. Each build starts once first, which writes its images into the base R tree
    it finds if they are missing; the probe loads a function from stats, so it opens a code image as
    well as the start-up image, and it must report no unused image. A package library gets code
    images only when a package is installed, so a package there that has only another build's code
    image stops the run with the list of packages to reinstall.
    """
    for binary in dict.fromkeys(binaries):
        runtime = runtime_of(binary)
        if runtime is not None and not file_contains(runtime, b"RY_IMAGE_LOG"):
            sys.exit(f"error: the runtime of {binary} ({runtime}) predates RY_IMAGE_LOG, so it cannot "
                     "report a start-up or code image it does not use, and its timings may measure a "
                     "slow start; compare a build that has it")
        with tempfile.TemporaryDirectory(prefix="ry-bench-probe-") as work:
            script = pathlib.Path(work) / "probe.R"
            script.write_text("invisible(stats::median(c(3, 1, 2)))\n")
            env = dict(os.environ, LC_ALL="C.UTF-8", RY_IMAGE_LOG="1")
            status, out, err, _, _ = spawn([binary, "run", "--runtime", "ry", script.name], env, cwd=work,
                                           check_images=False)
        if status != 0:
            sys.exit(f"error: {binary} cannot run a one-line R script: {failure_reason(out, err)}")
        unused = [line for line in err.splitlines() if line.startswith(IMAGE_UNUSED)]
        if unused:
            sys.exit(f"error: {binary} cannot use the images of the base R tree it finds, so it would "
                     "time a slow start:\n" + "\n".join(f"  {line}" for line in unused))
        identity = runtime_identity(runtime) if runtime is not None else None
        if identity is None:
            # A release carries its runtime inside itself, so its identity cannot be read here.
            continue
        libraries = [corpus.corpus / "library"]
        libraries += [pathlib.Path(part) for variable in ("R_LIBS", "R_LIBS_USER")
                      for part in os.environ.get(variable, "").split(os.pathsep) if part]
        stale = sorted({package.name for library in libraries if library.is_dir()
                        for package in library.iterdir() if foreign_code_image(package, identity)})
        if stale:
            sys.exit(f"error: {binary} (runtime {identity}) would run these packages without their "
                     "compiled code, because another build of ry installed them; reinstall them with "
                     "this build: " + ", ".join(stale))


def foreign_code_image(package, identity):
    """Whether the installed `package` has a code image of its lazy-load database, but none of the
    runtime build `identity`: `R/<name>.<build>.ryc`, or `R/<name>.ryc` from a ry that predates
    build-keyed names."""
    code = package / "R"
    images = list(code.glob(f"{glob_escape(package.name)}.*ryc"))
    return bool(images) and not (code / f"{package.name}.{identity}.ryc").is_file()


def glob_escape(text):
    return re.sub(r"([*?\[])", r"[\1]", text)


def runtime_of(binary):
    """The runtime library the ry build `binary` loads: `RY_LIBR`, or the one beside it; None for a
    build that carries its runtime inside itself."""
    if os.environ.get("RY_LIBR"):
        return pathlib.Path(os.environ["RY_LIBR"])
    beside = pathlib.Path(binary).resolve().with_name(runtime_library_name())
    return beside if beside.is_file() else None


def runtime_library_name():
    return {"Darwin": "libR.dylib", "Windows": "R.dll"}.get(platform.system(), "libR.so")


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
    samples_path = samples_file(args, suite)
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
        if samples_path is not None:
            print(f"samples in {samples_path}", flush=True)
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

def spawn(command, env, cwd=None, timeout=PROGRAM_TIMEOUT, check_images=True):
    """Runs a process to the end and returns its exit status, output, wall seconds and peak
    memory in MiB (`MEMORY_PARTS`, from `sample_memory`). The status is None when the process timed
    out or, with `check_images`, when ry reported an image it could not use: such a process timed a
    slow start, so its numbers do not count."""
    with tempfile.TemporaryFile("w+") as stdout, tempfile.TemporaryFile("w+") as stderr:
        started = time.monotonic()
        child = subprocess.Popen(command, stdout=stdout, stderr=stderr, text=True, env=env, cwd=cwd)
        expired, finished = threading.Event(), threading.Event()
        memory = {}

        def expire():
            expired.set()
            child.kill()

        timer = threading.Timer(timeout, expire)
        sampler = threading.Thread(target=sample_memory, args=(child.pid, finished, memory))
        timer.start()
        sampler.start()
        try:
            _, status = os.waitpid(child.pid, 0)
        finally:
            timer.cancel()
            finished.set()
            sampler.join()
        child.returncode = os.waitstatus_to_exitcode(status)
        seconds = time.monotonic() - started
        stdout.seek(0)
        stderr.seek(0)
        out, err = stdout.read(), stderr.read()
    if expired.is_set():
        return None, out, f"timed out after {timeout} s", seconds, memory
    unused = [line for line in err.splitlines()
              if line.startswith((IMAGE_UNUSED, IMAGE_WRITTEN))] if check_images else []
    if unused:
        return None, out, f"started without an image it should load ({unused[0]})", seconds, memory
    return child.returncode, out, err, seconds, memory


# The parts of a process's memory a result records, in MiB: the whole peak (`VmHWM`), then the
# peaks of anonymous memory outside the start-up image, of the image's resident pages and of
# `libR.so`'s. `wait4`'s `ru_maxrss` cannot give the whole peak: a child forked from this script
# starts with the script's own resident size as its high-water mark, and keeps it across `exec`.
MEMORY_PARTS = {"peak": "peak_mib", "anonymous": "anonymous_mib", "image": "image_mib", "libr": "libr_mib"}


def sample_memory(pid, finished, peaks):
    """Records in `peaks` the process's high-water resident size (`VmHWM`, which `exec` resets) and
    the largest anonymous, start-up image and `libR.so` resident sizes seen in `/proc/<pid>/smaps`,
    until `finished` is set. The image is mapped privately, so its pages a start writes are
    anonymous too; they count as the image's. Reading smaps takes about 1 ms for ry and 3 ms for
    GNU R, which is why it runs every 20 ms rather than more often, and so growth in a process's
    last 20 ms is missed and a process shorter than that has no sample."""
    while not finished.wait(0.02):
        try:
            with open(f"/proc/{pid}/status") as status:
                high_water = next(int(line.split()[1]) for line in status if line.startswith("VmHWM:"))
            with open(f"/proc/{pid}/smaps") as smaps:
                text = smaps.read()
        except (OSError, StopIteration):
            return
        peaks["peak"] = max(peaks.get("peak", 0.0), high_water / 1024)
        sizes = {"anonymous": 0, "image": 0, "libr": 0}
        mapping = ""
        for line in text.splitlines():
            if line.startswith("Rss:"):
                if mapping.endswith(".image"):
                    sizes["image"] += int(line.split()[1])
                elif mapping.endswith(runtime_library_name()):
                    sizes["libr"] += int(line.split()[1])
            elif line.startswith("Anonymous:"):
                if not mapping.endswith(".image"):
                    sizes["anonymous"] += int(line.split()[1])
            elif not line[:1].isupper():
                fields = line.split(None, 5)
                mapping = fields[5] if len(fields) > 5 else ""
        for part, kib in sizes.items():
            peaks[part] = max(peaks.get(part, 0.0), kib / 1024)


def memory_fields(memory):
    return {field: round(memory[part], 1) for part, field in MEMORY_PARTS.items() if part in memory}


def merge_memory(memories):
    """The peaks of several processes run one after another, part by part."""
    merged = {}
    for memory in memories:
        for part, mib in memory.items():
            merged[part] = max(merged.get(part, 0.0), mib)
    return merged


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
        status, out, err, _, memory = spawn(engine.command + [script.name], engine.env, cwd=folder)
        lines = [line for line in out.splitlines() if line.startswith(program["id"] + " ")]
        if status != 0 or not lines:
            return {"seconds": None, "reason": failure_reason(out, err)}
        _, checksum, median, _, cold = lines[-1].split()
        return {"seconds": float(median), "checksum": checksum, "cold": float(cold), **memory_fields(memory)}

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
        status, out, err, _, memory = spawn(engine.command + [str(script)], engine.env, cwd=source.parent)
        stem = source.name.removesuffix(".R")
        lines = [line for line in out.splitlines() if line.startswith(stem + " ")]
        if status != 0 or not lines:
            return {"seconds": None, "reason": failure_reason(out, err)}
        _, checksum, seconds = lines[-1].split()
        return {"seconds": float(seconds), "checksum": checksum, **memory_fields(memory)}

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
            seconds, memories, passed, failed = 0.0, [], 0, 0
            for script in sorted(tests.glob("*.R")):
                status, out, err, elapsed, memory = spawn(engine.command + [script.name], env, cwd=tests,
                                                          timeout=corpus.SUITE_TIMEOUT)
                if status not in (0, 1):
                    return {"seconds": None, "reason": f"{script.name}: {failure_reason(out, err)}"}
                seconds += elapsed
                memories.append(memory)
                passed += status == 0
                failed += status == 1
            return {"seconds": round(seconds, 3), "checksum": f"{passed} passed, {failed} failed, 0 errors",
                    "tests": {"passed": passed, "failed": failed, "errors": 0},
                    **memory_fields(merge_memory(memories))}
        driver = self.work / "driver.R"
        driver.write_text(corpus.driver(name, kind, tests))
        status, out, err, seconds, memory = spawn(engine.command + [str(driver)], env, cwd=tests,
                                                  timeout=corpus.SUITE_TIMEOUT)
        lines = [line for line in out.splitlines() if line.startswith("RESULT ")]
        if status != 0 or not lines:
            return {"seconds": None, "reason": failure_reason(out, err)}
        passed, failed, _, errors = (int(value) for value in lines[-1].split()[1:])
        return {"seconds": round(seconds, 3), "checksum": f"{passed} passed, {failed} failed, {errors} errors",
                "tests": {"passed": passed, "failed": failed, "errors": errors}, **memory_fields(memory)}

    def close(self):
        subprocess.run(["rm", "-rf", str(self.work)], check=True)


# Samples.

def samples_file(args, suite):
    if not args.samples:
        return None
    path = pathlib.Path(args.samples)
    return path if len(args.suites) == 1 else path.with_name(f"{path.stem}.{suite}{path.suffix or '.json'}")


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
    keys = {engine.key for engine in table}
    pairs = [(f"baseline{typed}", f"ry{typed}") for typed in ("", "_typed")
             if f"baseline{typed}" in keys and f"ry{typed}" in keys]
    paired_included = {new: [] for _, new in pairs}
    for program in programs:
        name = program["id"]
        row = {"id": name, "display": program["display"], "seconds": {}, "ratio": {}, "flags": [],
               **{field: {} for field in MEMORY_PARTS.values()}}
        if pairs:
            row["paired"] = {}
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
            for field in MEMORY_PARTS.values():
                sizes = [run[field] for run in runs if field in run]
                if sizes:
                    row[field][engine.key] = max(sizes)
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
        for old, new in pairs:
            ratios = paired(state, name, old, new)
            if not ratios or failures:
                continue
            median, low, high = interval(ratios, args.seed)
            row["paired"][new] = {"median": round(median, 4), "low": round(low, 4), "high": round(high, 4)}
            if not any(flag["kind"] == "too_short" and flag["engine"] in (old, new) for flag in row["flags"]):
                paired_included[new].append(ratios)
        if suite in TYPED_SUITES and "typed_source" in program and not failures:
            typed = next(engine for engine in table if engine.typed)
            reason = runner.typed_refusal(program, typed)
            if reason:
                row["flags"].append({"kind": "typed_refused", "engine": typed.key, "reason": reason})
        rows.append(row)
    def geomeans(included):
        means = {}
        for key, per_program in included.items():
            if per_program:
                point, low, high = geomean_interval(per_program, args.seed)
                means[key] = {"median": round(point, 4), "low": round(low, 4), "high": round(high, 4),
                              "programs": len(per_program)}
        return means
    results = {
        "schema": SCHEMA, "suite": suite, "name": manifest["name"],
        "meta": dict(meta, rounds=args.rounds, extra_rounds=args.extra_rounds, seed=args.seed),
        "engines": {engine.key: engine.label for engine in table},
        "programs": rows, "geomean": geomeans(included),
    }
    if pairs:
        results["paired_geomean"] = geomeans(paired_included)
    return results


def print_table(results, table):
    keys = [engine.key for engine in table if engine.key in results["engines"]]
    pairs = [key for key in keys if key in results.get("paired_geomean", {}) or
             any(key in row.get("paired", {}) for row in results["programs"])]
    label = results["engines"]
    print("| program | " + " | ".join(label[key] for key in keys) + " | "
          + " | ".join(f"GNU R / {label[key]}" for key in keys[1:])
          + "".join(f" | {label['baseline' + key.removeprefix('ry')]} / {label[key]}" for key in pairs)
          + " | flags |")
    print("| --- |" + " ---: |" * (2 * len(keys) - 1 + len(pairs)) + " --- |")
    for row in results["programs"]:
        cells = [f"{row['seconds'][key]:.3f} s" if key in row["seconds"] else "" for key in keys]
        for ratio in [row["ratio"].get(key) for key in keys[1:]] + [row.get("paired", {}).get(key) for key in pairs]:
            cells.append(f"{ratio['median']:.2f}x ({ratio['low']:.2f} to {ratio['high']:.2f})" if ratio else "")
        flags = ", ".join(f"{flag['kind']} ({flag['engine']})" for flag in row["flags"])
        if row.get("failures"):
            flags = ", ".join(["FAILED"] + ([flags] if flags else []))
        print(f"| {row['display']} | " + " | ".join(cells) + f" | {flags} |")
    print("\nPeak memory in MiB: whole process / anonymous / start-up image / libR.so resident\n")
    print("| program | " + " | ".join(label[key] for key in keys) + " |")
    print("| --- |" + " ---: |" * len(keys))
    for row in results["programs"]:
        cells = [" / ".join(f"{row[field][key]:.1f}" if key in row.get(field, {}) else "-"
                            for field in MEMORY_PARTS.values()) for key in keys]
        print(f"| {row['display']} | " + " | ".join(cells) + " |")
    for key, mean in results["geomean"].items():
        print(f"\nGeometric mean of GNU R / {label[key]} over {mean['programs']} programs: "
              f"{mean['median']:.2f}x (95% interval {mean['low']:.2f} to {mean['high']:.2f})")
    for key, mean in results.get("paired_geomean", {}).items():
        print(f"\nGeometric mean of {label['baseline' + key.removeprefix('ry')]} / {label[key]} over "
              f"{mean['programs']} programs: {mean['median']:.2f}x "
              f"(95% interval {mean['low']:.2f} to {mean['high']:.2f}; above 1 means {label[key]} is faster)")
    for build, size in results["meta"].get("libr_text_bytes", {}).items():
        print(f"\nlibR.so .text of {build}: {size / 1024 / 1024:.2f} MiB ({size} bytes)")


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


def runtime_identity(library):
    """The identity of the runtime library `library`, which names every image it writes."""
    import ctypes
    function = ctypes.CDLL(str(library), mode=os.RTLD_LOCAL).ry_runtime_id
    function.restype = ctypes.c_char_p
    return function().decode()


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
    text = {build: text_size(runtime_of(binary))
            for build, binary in (("ry", args.ry), ("baseline", args.baseline)) if binary and runtime_of(binary)}
    return {
        "date": time.strftime("%Y-%m-%d"),
        "ry_commit": commit(args.ry),
        "libr_text_bytes": {build: size for build, size in text.items() if size is not None},
        "too_short_seconds": SHORT,
        "gnu_r": {"version": version.strip(), "blas": (blas or lapack).strip()},
        "machine": {"cpu": cpu, "cores": os.cpu_count(), "kernel": f"{platform.system()} {platform.release()}"},
        "command": "tools/bench/bench.py " + " ".join(shlex.quote(argument) for argument in sys.argv[1:]),
    }


def text_size(library):
    """The size in bytes of the `.text` section of the runtime library `library` (`size -A`), or
    None where `size` is missing."""
    try:
        listing = subprocess.run(["size", "-A", str(library)], capture_output=True, text=True, check=True).stdout
    except (OSError, subprocess.CalledProcessError):
        return None
    return next((int(line.split()[1]) for line in listing.splitlines() if line.startswith(".text ")), None)


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
