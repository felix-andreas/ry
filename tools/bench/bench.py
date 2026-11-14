#!/usr/bin/env python3
"""Runs ry's published benchmark suites and writes each suite's results.json. Every suite but one
times ry against GNU R, and the typed suite times typed ry against Rust.

The suites, each described by the suite.toml in its directory:

  public        Public R JIT benchmarks: the 54 Ř and rcp programs, vendored under public/.
  packages      Package test suites: the pinned CRAN packages of packages/lock.tsv.
  kernels       Numeric kernels: our loops, recursion, arrays and numeric math.
  applications  Application workloads: our calls, dispatch, conditions, records and tables.
  codebases     The three 300,000-line benchmark code bases, each running its test suite.
  hot           Hot package entries: the package calls that dominate the code bases, replayed in a loop.
  typed         Typed ry against Rust: every typed kernel and application beside its Rust twin.

In the first four the engines are GNU R, ry, ry typed, and ry typed with `--aot`: a program whose
suite.toml entry names a `typed_source` (its own `<name>.typed.R`, or a variant's identical one)
also runs that typed version in strict mode, once compiled at each function's first call and once
compiled before the program starts (`ry run --aot`). Every run is a fresh process. A round runs every program on
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

Compiling ahead moves a program's compile time; it does not remove it. A public program times
itself, so with `--aot` its clock would leave out the compile that the other engines' clocks hold.
The runner therefore adds the time `RY_TYPED_LOG` reports for the compile before the program starts
to that clock, and the column counts the same work as the others. The kernels and applications
harnesses time warm runs, which no compile is part of on either typed engine, and they also time
the first run of the program in the process. A row stores that as `first_seconds`, the median over
rounds per engine, which is where compiling ahead shows: the first run compiles nothing.

The typed suite also stores each program's `compile_seconds`: what `rustc` took to compile the Rust
twin, and what the typed core took to compile the typed program, which is the sum of the times in
the `compiled for ... in N us` and `refused ... after N us` lines of that same `RY_TYPED_LOG` run.

The published numbers measure the official release archive itself, so `--write` takes
`--release ARCHIVE`, the Linux x86_64 archive of a tagged release (`ry-x86_64-unknown-linux-gnu.tar.gz`).
The runner unpacks it into a private directory with its own cache directory, where the archive's
`ry` extracts the runtime and base R it embeds, and results.json records the release tag and the
archive's sha256. A nightly archive runs too, but `--write` refuses it. Without `--release`, `--ry`
names the build under test.

Every suite checks before it runs that the corpus library holds each package its programs load, at
the version lock.tsv pins (for the packages suite, every locked package), and stops with the list of
missing ones. `--partial` runs anyway: the summary and results.json say the run is partial and name
the missing packages, and `--write` refuses such a run.

A start-up image and a package's code image load only into the build of ry that wrote them, and a
build that cannot load them starts the slow way: a start that calls `stats::median` runs 8 times the
instructions (306 million rather than 37) and peaks 9 MiB higher. Builds that share a base R tree
each keep their own images there (crates/rcmd/src/images.rs), and a build writes its images the
first time it starts. Before the first suite, bench.py starts each ry build once, so no timed
process pays for writing them, and stops when a package in the corpus library or `R_LIBS` has only
another build's code image, naming the packages to reinstall (`refuse_foreign_images`). Every ry
process runs with `RY_IMAGE_LOG=1`, and one that reports an unused image or writes images fails, so
a run never compares a fast start with a slow one. A build whose runtime predates `RY_IMAGE_LOG`
cannot report one, so bench.py refuses it.

The typed suite needs rustc and no GNU R. Its engines are Rust, the baseline, and ry typed: each
program's `<name>.typed.R` runs as in the kernels and applications suites, and its Rust twin
(`typed/<name>.rs`, compiled once per run with `rustc -O`) reports through `typed/harness.rs`, which
takes the same samples and prints the same line as harness.typed.R. A ratio there is typed ry's time
over Rust's, so 1x is Rust's speed and lower is faster, and a program counts as too short only when
Rust finishes it in under 0.1 ms, because both harnesses time a warm run inside the process with a
clock that resolves well under a microsecond. Both must print the same checksum.

Without `--release`, `--ry` must name a release build. `--any-build` also takes an iterate or debug
build, for a quick look while changing ry: the output says which build it timed, and no number from
it is a release number.

`--samples FILE` keeps every round's raw samples in FILE as it goes, and a later run with the same
FILE and options resumes after the last complete round. A run of several suites keeps each suite's
in FILE with the suite's name before its extension (`samples.kernels.json`). `--write` needs at least 10 rounds and
every program, and stores results.json; tools/bench/readme.py renders the README from it.

The hot suite (hot/suite.toml, which tooling/hot.py generates from a capture of the code bases)
runs differently: one process per engine and round runs every selected program through
hot/harness.R, so packages load once, and the harness times each program's batches of calls
inside the process, so a program counts as too short only under 1 ms. Each code base also gets a
predicted speed-up: the programs' speed-ups averaged with the weights the suite gives them in that
code base (`hot_summary`). Two passes run on ry alone after the rounds: `--breakdown` reads where the
runtime spent each program's time from its region profile, and `--instructions` counts each
program's instructions under callgrind, which `--save-instructions FILE` keeps and
`--baseline-instructions FILE` (or a `--baseline` build) compares against. `--functions N` adds
the N functions of the runtime and the packages' compiled code that hold the most of each code
base's instructions, which needs a build with symbols (`cargo build --profile profiling`).

Usage: tools/bench/bench.py SUITE... | all [--release ARCHIVE | --ry PATH] [--rscript PATH] [--rounds N] [--seed N]
                            [--filter TEXT] [--extra-rounds N] [--samples FILE] [--write]
                            [--baseline PATH] [--jit-off] [--partial] [--any-build]
                            [--breakdown] [--instructions] [--save-instructions FILE]
                            [--baseline-instructions FILE] [--functions N]
"""
import argparse, atexit, hashlib, json, math, os, pathlib, platform, random, re, shlex, shutil
import statistics, subprocess, sys, tarfile, tempfile, threading, time, tomllib

here = pathlib.Path(__file__).resolve().parent
root = here.parent.parent
sys.path.insert(0, str(here / "tooling"))
sys.path.insert(0, str(here))
import packages as corpus  # noqa: E402
from evict import evict  # noqa: E402

SUITES = ["public", "packages", "kernels", "applications", "codebases", "hot", "typed"]
# The suites whose programs may have a typed version, which the `ry typed` engine runs.
TYPED_SUITES = ("public", "kernels", "applications")
# The suite that times typed ry against Rust, where every other suite times ry against GNU R.
RUST_SUITE = "typed"
# The suite of captured package entries, whose runner times every program in one process.
HOT_SUITE = "hot"
# `rustc -O` is opt-level 3, the level of cargo's release profile, and names no target CPU, so a
# Rust twin is built the way a released Rust program is.
RUSTC_FLAGS = ["-O", "--edition", "2024"]
SCHEMA = 1
# The fewest rounds a published run may have: fewer cannot separate a 10% change from noise on a
# shared cloud machine, where one fixed C loop varies by about 25% between processes.
PUBLISHED_ROUNDS = 10
# Below this many seconds a time mostly measures start-up and the timer.
SHORT = 0.1
# The codebases suite compares each test block's time, and only blocks GNU R runs in at least this
# long count: a shorter one measures the timer.
PER_TEST_FLOOR = 0.001
# A code base's run takes minutes on each engine.
CODEBASE_TIMEOUT = 7200
# The hot suite's harness times batches of about 20 ms of GNU R work inside the process, with a
# clock that resolves to the microsecond on both engines, so a batch under a millisecond measures
# the clock.
HOT_SHORT = 0.001
# One process runs every program of the suite, and under callgrind ry runs about 50 times slower.
HOT_TIMEOUT = 3600
# The same floor for the typed suite. Both of its harnesses time a warm run inside the process, so
# start-up is not in the time, and both print it to the microsecond: 0.1 ms is 100 of those steps.
RUST_SHORT = 0.0001
# An interval reaching further than this from its median is too wide to quote.
WIDE = 0.10
PROGRAM_TIMEOUT = 300
# The lines ry prints on standard error under RY_IMAGE_LOG for each start-up or code image it could
# not use (crates/interp/src/image.rs, `report_unused`), and when it writes a build's images into a
# base R tree (crates/rcmd/src/rcmd.rs, `refresh_start_image`).
IMAGE_UNUSED = "ry: image unused: "
IMAGE_WRITTEN = "ry: image written: "
STRICT_CONFIG = "[check]\ntyping = true\nstrict = true\n"
# A typed call that first runs in the interpreter to observe an R value's type logs this, then
# compiles on its next call, so the line is not a refusal.
AWAITING = "it waits on the interpreter to run `"
# A `RY_TYPED_LOG` line for a function the typed core compiled ends with the time that took.
COMPILED = re.compile(r"(?:compiled for .* in|refused .* after) (?P<microseconds>\d+) us$")
# The `RY_TYPED_LOG` line `ry run --aot` prints once it has compiled a script's functions, before
# the script's first expression runs (crates/typed/src/ahead.rs).
COMPILED_AHEAD = re.compile(r"^typed: compiled \d+ functions? ahead of time in (?P<microseconds>\d+) us$", re.M)


def main():
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("suites", nargs="+", metavar="SUITE", help=f"{', '.join(SUITES)} or all")
    build = parser.add_mutually_exclusive_group()
    build.add_argument("--ry", default=str(root / "target/release/ry"), help="the ry build under test")
    build.add_argument("--release", metavar="ARCHIVE", type=pathlib.Path,
                       help="the release archive under test, which --write requires")
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
    parser.add_argument("--any-build", action="store_true",
                        help="also time a ry that is not a release build, and say so in the output")
    parser.add_argument("--breakdown", action="store_true",
                        help="hot suite: one more ry process that profiles where each program spends its time")
    parser.add_argument("--instructions", action="store_true",
                        help="hot suite: count each program's instructions on ry under callgrind")
    parser.add_argument("--save-instructions", metavar="FILE", help="hot suite: keep the instruction counts in FILE")
    parser.add_argument("--functions", metavar="N", type=int, default=0,
                        help="hot suite: with --instructions, the N functions holding the most of each code base's instructions")
    parser.add_argument("--baseline-instructions", metavar="FILE",
                        help="hot suite: compare the instruction counts with those of an earlier --save-instructions")
    args = parser.parse_args()
    # Programs run from their own directories.
    args.ry = os.path.abspath(args.ry)
    args.baseline = args.baseline and os.path.abspath(args.baseline)
    suites = SUITES if "all" in args.suites else args.suites
    args.suites = suites
    for suite in suites:
        if suite not in SUITES:
            sys.exit(f"error: no suite {suite!r}; the suites are {', '.join(SUITES)}")
    if (args.save_instructions or args.baseline_instructions or args.functions) and not args.instructions:
        sys.exit("error: --save-instructions, --baseline-instructions and --functions go with --instructions")
    if (args.breakdown or args.instructions) and suites != [HOT_SUITE]:
        sys.exit(f"error: --breakdown and --instructions measure the {HOT_SUITE} suite alone")
    if args.rounds < 0 or (args.rounds == 0 and not (args.breakdown or args.instructions)):
        sys.exit("error: --rounds must be at least 1, or 0 for only a --breakdown or --instructions pass")
    release = None
    if args.release:
        args.ry, release = unpack_release(args.release)
    if args.write:
        if release is None:
            sys.exit("error: --write records the official release archive; pass --release ARCHIVE, "
                     "the ry-x86_64-unknown-linux-gnu.tar.gz of the tagged release")
        if release["nightly"]:
            sys.exit(f"error: --write records a tagged release, and {release['tag']} is a nightly build")
        if args.rounds < PUBLISHED_ROUNDS:
            sys.exit(f"error: --write needs at least {PUBLISHED_ROUNDS} rounds")
        if args.filter or args.baseline or args.jit_off:
            sys.exit("error: --write records every program on the published engines only")
        if args.partial:
            sys.exit("error: --write records complete runs only, so it does not take --partial")
        changes = changed_paths(str(root))
        if changes:
            sys.exit("error: --write records a clean commit of the benchmark programs, but the working "
                     "tree has changes; commit or remove them:\n" + "\n".join(f"  {line}" for line in changes))
    unreleased = [f"{binary} is not a release build ({version})"
                  for binary in filter(None, [args.ry, args.baseline]) if (version := unreleased_build(binary))]
    if unreleased and not args.any_build:
        sys.exit("error: " + "; ".join(unreleased) + "; benchmarks time release builds only, so build it "
                 "with `cargo build --release -p ry-lang -p libr`, or pass --any-build for a quick look")
    refuse_foreign_images(list(filter(None, [args.ry, args.baseline])))
    manifests = {suite: load_suite(suite) for suite in suites}
    missing = {suite: missing_packages(suite, selected(manifest, args), args.rscript) for suite, manifest in manifests.items()}
    if any(missing.values()) and not args.partial:
        sys.exit("error: the corpus library lacks packages the suites load; run "
                 "`just bench-packages install`, or pass --partial to run without them:\n"
                 + "\n".join(f"  {suite}: {', '.join(names)}" for suite, names in missing.items() if names))
    meta = metadata(args, release)
    # Each suite records only the engine it compares against, and a run of the typed suite alone
    # needs no GNU R.
    baselines = {}
    if any(suite != RUST_SUITE for suite in suites):
        baselines["gnu_r"] = gnu_r_metadata(args)
    if RUST_SUITE in suites:
        baselines["rust"] = rust_metadata()
    print(f"Machine: {meta['machine']['cpu']}, {meta['machine']['cores']} cores, {meta['machine']['kernel']}")
    measured = f"release {release['tag']} (sha256 {release['sha256']})" if release else meta["ry_commit"]
    against = [f"GNU R: {baselines['gnu_r']['version']}, BLAS {baselines['gnu_r']['blas']}"] if "gnu_r" in baselines else []
    against += [f"Rust: {baselines['rust']['version']}, flags {baselines['rust']['flags']}"] if "rust" in baselines else []
    print("; ".join(against + [f"ry {measured}"]))
    warning = "NOT A RELEASE BUILD: " + "; ".join(unreleased) + ". Quote none of these numbers." if unreleased else None
    print(f"{warning}\n" if warning else "")
    failed = False
    for suite in suites:
        key = baseline_key(suite)
        failed |= run_suite(suite, manifests[suite], missing[suite], args, dict(meta, **{key: baselines[key]}))
        if warning:
            print(f"{warning}\n")
    if failed:
        sys.exit("error: a program failed or disagreed with its suite's baseline engine; nothing was written")


# Suites and engines.

class Engine:
    def __init__(self, key, label, command, env, typed=False, ahead=False):
        self.key, self.label, self.command, self.env, self.typed = key, label, command, env, typed
        # Whether the engine compiles a program's typed functions before the program starts.
        self.ahead = ahead


def baseline_key(suite):
    """The engine a suite's ratios compare against, which is first in its engine table."""
    return "rust" if suite == RUST_SUITE else "gnu_r"


def ratio_kind(suite):
    """How a suite's ratios read. A `speedup` is the baseline's time over the engine's, so above 1x
    the engine is faster than GNU R. A `slowdown` is the engine's time over the baseline's, so 1x
    is Rust's speed and lower is faster."""
    return "slowdown" if suite == RUST_SUITE else "speedup"


def short_floor(suite):
    return {RUST_SUITE: RUST_SHORT, HOT_SUITE: HOT_SHORT}.get(suite, SHORT)


def engines(args, suite):
    base = {key: value for key, value in os.environ.items() if key not in ("RY_JIT", "RY_TYPED_LOG")}
    base.update(LC_ALL="C.UTF-8")
    if suite == RUST_SUITE:
        # The typed programs load no package, so neither engine gets a library, and `--jit-off`
        # does not apply: a strict typed program is compiled before it runs.
        ry_env = dict(base, RY_IMAGE_LOG="1")
        table = [Engine("rust", "Rust", None, base),
                 Engine("ry_typed", "ry typed", [args.ry, "run", "--runtime", "ry"], ry_env, typed=True)]
        if args.baseline:
            table.append(Engine("baseline_typed", "baseline typed", [args.baseline, "run", "--runtime", "ry"],
                                ry_env, typed=True))
        return table
    library = codebase_library() if suite in ("codebases", HOT_SUITE) else corpus.corpus / "library"
    base.update(library_environment(args.rscript, library))
    ry = [args.ry, "run", "--runtime", "ry"]
    ry_env = dict(base, RY_IMAGE_LOG="1")
    table = [Engine("gnu_r", "GNU R", [args.rscript, "--vanilla"], base),
             Engine("ry", "ry", ry, ry_env)]
    typed = suite in TYPED_SUITES
    if typed:
        table.append(Engine("ry_typed", "ry typed", ry, ry_env, typed=True))
        table.append(Engine("ry_typed_aot", "ry typed, AOT", ry + ["--aot"], ry_env, typed=True, ahead=True))
    if args.jit_off or suite == "codebases":
        table.append(Engine("ry_nojit", "ry, JIT off", ry, dict(ry_env, RY_JIT="off")))
    if args.baseline:
        baseline = [args.baseline, "run", "--runtime", "ry"]
        baseline_env = dict(base, RY_IMAGE_LOG="1")
        table.append(Engine("baseline", "baseline", baseline, baseline_env))
        if typed:
            table.append(Engine("baseline_typed", "baseline typed", baseline, baseline_env, typed=True))
    return table


def library_environment(rscript, library):
    """The variables that make both engines load packages from `library` and then base R's own,
    so neither reads a different build of a package from a site library; none when there is no
    such library."""
    if not library.is_dir():
        return {}
    return {"R_LIBS": str(library), "R_LIBS_USER": str(library), "R_LIBS_SITE": gnu_r_library(rscript)}


def gnu_r_library(rscript):
    """GNU R's own library, which holds base R's packages and whatever was installed beside them."""
    return subprocess.run([rscript, "--vanilla", "-e", "cat(.Library)"],
                          capture_output=True, text=True, check=True).stdout.strip()


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
    image stops the run with the list of packages to reinstall. A release carries its runtime inside
    itself and extracts it on its first start, so its runtime is read from the R home it reports,
    whose `.libr` names the runtime library on its first line.
    """
    for binary in dict.fromkeys(binaries):
        with tempfile.TemporaryDirectory(prefix="ry-bench-probe-") as work:
            script = pathlib.Path(work) / "probe.R"
            script.write_text("invisible(stats::median(c(3, 1, 2)))\ncat(R.home())\n")
            env = dict(os.environ, LC_ALL="C.UTF-8", RY_IMAGE_LOG="1")
            status, out, err, _, _ = spawn([binary, "run", "--runtime", "ry", script.name], env, cwd=work,
                                           check_images=False)
        if status != 0:
            sys.exit(f"error: {binary} cannot run a two-line R script: {failure_reason(out, err)}")
        runtime = runtime_of(binary)
        if runtime is None:
            home = pathlib.Path(out.strip())
            try:
                runtime = pathlib.Path((home / ".libr").read_text().splitlines()[0])
            except (OSError, IndexError) as error:
                sys.exit(f"error: {binary} reported the R home {home}, which names no runtime library: {error}")
        if not file_contains(runtime, b"RY_IMAGE_LOG"):
            sys.exit(f"error: the runtime of {binary} ({runtime}) predates RY_IMAGE_LOG, so it cannot "
                     "report a start-up or code image it does not use, and its timings may measure a "
                     "slow start; compare a build that has it")
        unused = [line for line in err.splitlines() if line.startswith(IMAGE_UNUSED)]
        if unused:
            sys.exit(f"error: {binary} cannot use the images of the base R tree it finds, so it would "
                     "time a slow start:\n" + "\n".join(f"  {line}" for line in unused))
        identity = runtime_identity(runtime)
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
    if suite == HOT_SUITE:
        return load_hot_suite(here / suite)
    manifest = tomllib.loads((here / suite / "suite.toml").read_text())
    if suite == RUST_SUITE:
        check_rust_suite(manifest)
        return manifest
    for program in manifest["programs"]:
        source = root / program["source"] if "source" in program else None
        if source is not None and not source.exists():
            sys.exit(f"error: {suite}/suite.toml names {program['source']}, which does not exist")
        if suite in TYPED_SUITES:
            # A variant whose typed version would repeat another program's may name that
            # program's file, but a `<id>.typed.R` of its own is always its typed source.
            own = source.with_name(f"{program['id']}.typed.R")
            named = program.get("typed_source")
            if (named is None and own.exists()) or (named is not None and own.exists() and root / named != own):
                sys.exit(f"error: {suite}/suite.toml and the files disagree on whether {program['id']} has a typed version")
            if named is not None and not (root / named).exists():
                sys.exit(f"error: {suite}/suite.toml names {named}, which does not exist")
    listed = {program["id"] for program in manifest["programs"]}
    if suite in ("kernels", "applications"):
        present = {path.name.removesuffix(".R") for path in (here / suite).glob("*.R")
                   if not path.name.endswith(".typed.R")}
        if present != listed:
            sys.exit(f"error: {suite}/suite.toml lists {sorted(listed - present)} without a file and "
                     f"misses {sorted(present - listed)}")
    return manifest


def load_hot_suite(directory):
    """The hot suite's manifest, after checking that every sample it names is there and that each
    program gives every sample its iterations and its rng, threw and fresh flags."""
    path = directory / "suite.toml"
    if not path.exists():
        sys.exit(f"error: the {HOT_SUITE} suite has not been generated: {path} does not exist. It comes from a "
                 "capture of the benchmark code bases on ry; generate them with tools/bench/tooling/generate.py, "
                 "then run `just bench-hot-capture DIR` on that directory")
    manifest = tomllib.loads(path.read_text())
    if not isinstance(manifest.get("session"), list):
        sys.exit(f"error: {path} does not list the packages a replay loads (`session`)")
    for program in manifest["programs"]:
        count = len(program["samples"])
        if not count or any(len(program.get(key, ())) != count for key in ("iterations", "rng", "threw", "fresh")):
            sys.exit(f"error: {path} gives {program['id']} {count} samples but not as many iterations, rng, threw and fresh flags")
        for sample in program["samples"]:
            if not (directory / sample).is_file():
                sys.exit(f"error: {path} names {sample}, which does not exist")
    return manifest


def check_rust_suite(manifest):
    """Exits unless the typed suite lists exactly the typed kernels and applications, each under
    the name its own suite gives it and each with a Rust twin."""
    owners = {}
    for owner in ("kernels", "applications"):
        for program in tomllib.loads((here / owner / "suite.toml").read_text())["programs"]:
            if "typed_source" in program:
                owners[program["id"]] = program
    listed = {program["id"]: program for program in manifest["programs"]}
    if set(listed) != set(owners):
        sys.exit(f"error: {RUST_SUITE}/suite.toml lists {sorted(set(listed) - set(owners))} that are not typed "
                 f"kernels or applications and misses {sorted(set(owners) - set(listed))}")
    twins = {path.stem for path in (here / RUST_SUITE).glob("*.rs")} - {"harness"}
    if twins != set(listed):
        sys.exit(f"error: {RUST_SUITE}/suite.toml lists {sorted(set(listed) - twins)} without a Rust twin and "
                 f"misses {sorted(twins - set(listed))}")
    for name, program in listed.items():
        owner = owners[name]
        expected = {"display": owner["display"], "typed_source": owner["typed_source"],
                    "rust_source": f"tools/bench/{RUST_SUITE}/{name}.rs"}
        for key, value in expected.items():
            if program.get(key) != value:
                sys.exit(f"error: {RUST_SUITE}/suite.toml gives {name} the {key} {program.get(key)!r}, "
                         f"which must be {value!r}")


def selected(manifest, args):
    return [program for program in manifest["programs"]
            if args.filter in program["id"] or args.filter == program.get("group")]


def missing_packages(suite, programs, rscript):
    """The packages the programs load that the corpus library lacks at the locked version, each
    with the reason. A package's test suite may load any locked package, so the packages suite
    needs all of them."""
    if suite == HOT_SUITE:
        # The captured code bases import the dependency closure, which pins no versions of its own.
        # A package installed beside base R is not installed into the closure again, and both
        # engines find it there (`library_environment`).
        needed = {name for program in programs for name in program["namespaces"]} - corpus.BASE_PACKAGES
        libraries = [codebase_library(), pathlib.Path(gnu_r_library(rscript))] if needed else []
        return sorted((name for name in needed
                       if not any((library / name / "DESCRIPTION").is_file() for library in libraries)), key=str.lower)
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
    runners = {"public": PublicRunner, "packages": PackageRunner, "codebases": CodebaseRunner, RUST_SUITE: RustRunner,
               HOT_SUITE: HotRunner}
    runner = runners.get(suite, WorkloadRunner)(suite, args)
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
                if program["id"] not in state["extra"] and wide_interval(state, program["id"], table, ratio_kind(suite))]
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
        if suite == HOT_SUITE:
            hot_passes(runner, programs, table, args, results)
    finally:
        runner.close()
    print_table(results, table)
    if suite == HOT_SUITE:
        print_hot(results)
    for key, found in results["worst"].items():
        print(f"Worst program, {results['engines'][key]}: {found['id']} at {found['ratio']:.2f}x")
    for row in results["programs"]:
        for key, found in row.get("per_block", {}).items():
            print(f"{row['display']}, {results['engines'][key]}: per-block geometric mean of GNU R / engine "
                  f"{found['geomean']:.2f}x over {found['blocks']} of {found['of']} blocks")
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
    measured = {program["id"]: {} for program, _ in plan}
    if getattr(runner, "batched", False):
        # One process per engine runs the round's programs, in the round's order, and the engines
        # take turns in the order the round drew for its first program.
        for engine in plan[0][1] if plan else []:
            programs = [program for program, engines_here in plan if engine in engines_here]
            for name, result in runner.run_batch(programs, engine).items():
                measured[name][engine.key] = result
    else:
        for program, engines_here in plan:
            for engine in engines_here:
                measured[program["id"]][engine.key] = runner.run(program, engine)
    for name, results in measured.items():
        line = " ".join(f"{key} {describe(result)}" for key, result in sorted(results.items()))
        print(f"  {name}: {line}", flush=True)
    return measured


def describe(result):
    if result.get("seconds") is None:
        return f"fails ({result.get('reason', '')[:80]})"
    return duration(result["seconds"]).replace(" ", "")


def duration(seconds):
    """A time for a person: seconds, or milliseconds for a time under a tenth of a second, which
    three decimals of a second would round to nothing."""
    return f"{seconds:.3f} s" if seconds >= SHORT else f"{seconds * 1000:.3f} ms"


# Running one program on one engine.

def spawn(command, env, cwd=None, timeout=PROGRAM_TIMEOUT, check_images=True, sample=True):
    """Runs a process to the end and returns its exit status, output, wall seconds and peak
    memory in MiB (`MEMORY_PARTS`, from `sample_memory`; empty without `sample`). The status is
    None when the process timed out or, with `check_images`, when ry reported an image it could
    not use: such a process timed a slow start, so its numbers do not count."""
    with tempfile.TemporaryFile("w+") as stdout, tempfile.TemporaryFile("w+") as stderr:
        started = time.monotonic()
        child = subprocess.Popen(command, stdout=stdout, stderr=stderr, text=True, env=env, cwd=cwd)
        expired, finished = threading.Event(), threading.Event()
        memory = {}

        def expire():
            expired.set()
            child.kill()

        timer = threading.Timer(timeout, expire)
        sampler = threading.Thread(target=sample_memory, args=(child.pid, finished, memory)) if sample else None
        timer.start()
        if sampler:
            sampler.start()
        try:
            _, status = os.waitpid(child.pid, 0)
        finally:
            timer.cancel()
            finished.set()
            if sampler:
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
    any other checksum must match exactly. A hot program prints one checksum per sample, joined by
    commas, so it is compared part by part. A sample's checksum (hot/replay.R) is
    `<kind>:<length>:<hash>:<magnitude>:<balance>`: the kind, the length and the hash of
    everything exact (types, lengths, non-finite counts, text, attribute names) must match, and the
    two sums over the numbers at every depth agree to an absolute 1e-10 or a relative 1e-13."""
    values, references = value.split(","), reference.split(",")
    return len(values) == len(references) and all(map(same_checksum_part, values, references))


HOT_CHECKSUM_KINDS = {"lgl", "int", "dbl", "cpl", "chr", "raw", "list", "s4"}


def same_checksum_part(value, reference):
    fields, expected = value.split(":"), reference.split(":")
    if len(fields) == len(expected) == 5 and fields[0] in HOT_CHECKSUM_KINDS:
        try:
            return (fields[:3] == expected[:3]
                    and all(math.isclose(float(a), float(b), rel_tol=1e-13, abs_tol=1e-10)
                            for a, b in zip(fields[3:], expected[3:])))
        except ValueError:
            return value == reference
    try:
        number, expected = float(value), float(reference)
    except ValueError:
        return value == reference
    return value == reference or math.isclose(number, expected, rel_tol=1e-10, abs_tol=1e-300)


def interpreted(line):
    """The function a `RY_TYPED_LOG` line says runs in the interpreter, or the line itself. The
    harness's own functions log a reason that quotes the program's argument, so the harness
    baseline matches them by name."""
    name, marker, _ = line.removeprefix("typed: ").partition(" runs in the interpreter: ")
    return name if marker else line


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
        self.typed_compile = {}

    def run(self, program, engine):
        if engine.typed:
            source, harness, folder = root / program["typed_source"], self.typed_harness, self.work / "typed"
        else:
            source, harness, folder = root / program["source"], self.harness, self.work / "plain"
        script = folder / f"{program['id']}.R"
        script.write_text(harness + "\n" + source.read_text())
        return self.reported(program, *spawn(engine.command + [script.name], engine.env, cwd=folder))

    def reported(self, program, status, out, err, _, memory):
        """What a finished process's harness line says: the checksum and the median warm time."""
        lines = [line for line in out.splitlines() if line.startswith(program["id"] + " ")]
        if status != 0 or not lines:
            return {"seconds": None, "reason": failure_reason(out, err)}
        _, checksum, median, _, cold = lines[-1].split()
        return {"seconds": float(median), "checksum": checksum, "first": float(cold), **memory_fields(memory)}

    def typed_log(self, text, engine):
        """What `RY_TYPED_LOG` reports for one run of a typed source under the harness: the refusal
        lines, and the seconds the typed core spent compiling, summed over its `compiled for ... in
        N us` and `refused ... after N us` lines. Measured outside the timed rounds, since logging
        costs time."""
        script = self.work / "typed" / "refusals.R"
        script.write_text(self.typed_harness + "\n" + text)
        env = dict(engine.env, RY_TYPED_LOG="1", RY_BENCH_REPEAT="1")
        status, out, err, _, _ = spawn(engine.command + [script.name], env, cwd=self.work / "typed")
        if status != 0:
            sys.exit(f"error: the typed log run failed: {failure_reason(out, err)}")
        lines = [line for line in err.splitlines() if line.startswith("typed: ") and AWAITING not in line]
        microseconds = sum(int(match["microseconds"]) for match in map(COMPILED.search, lines) if match)
        return [line for line in lines if not COMPILED.search(line)], microseconds / 1e6

    def typed_refusal(self, program, engine):
        """The refusals the typed program logs beyond those of the harness alone, or None."""
        if "" not in self.refusals:
            empty, _ = self.typed_log('measure("empty", function() Num.to_f64(0))\n', engine)
            self.refusals[""] = {interpreted(line) for line in empty}
        refusals, self.typed_compile[program["id"]] = self.typed_log((root / program["typed_source"]).read_text(), engine)
        lines = [line for line in refusals if interpreted(line) not in self.refusals[""]]
        return "; ".join(dict.fromkeys(line.removeprefix("typed: ") for line in lines)) or None

    def close(self):
        for path in sorted(self.work.rglob("*"), reverse=True):
            path.rmdir() if path.is_dir() else path.unlink()
        self.work.rmdir()


class RustRunner(WorkloadRunner):
    """The typed suite: a program's `<name>.typed.R` as WorkloadRunner runs it, and its Rust twin,
    compiled on its first run and timed by typed/harness.rs, which prints the same line."""

    def __init__(self, suite, args):
        super().__init__(suite, args)
        self.rustc = rustc()
        self.binaries = {}
        self.rust_compile = {}

    def run(self, program, engine):
        if engine.key != "rust":
            return super().run(program, engine)
        return self.reported(program, *spawn([str(self.binary(program))], engine.env, cwd=self.work))

    def binary(self, program):
        """The compiled Rust twin, built on first use and timed as it is built."""
        name = program["id"]
        if name not in self.binaries:
            binary = self.work / f"rust-{name}"
            started = time.monotonic()
            built = subprocess.run([self.rustc, *RUSTC_FLAGS, "-o", str(binary), str(root / program["rust_source"])],
                                   capture_output=True, text=True)
            if built.returncode != 0:
                sys.exit(f"error: rustc does not compile {program['rust_source']}:\n{built.stderr}")
            self.rust_compile[name] = time.monotonic() - started
            self.binaries[name] = binary
        return self.binaries[name]

    def compile_seconds(self, program):
        """What each engine spent compiling the program: `rustc` on the Rust twin, one process from
        start to finish, and the typed core on the typed program, summed over the functions it
        compiled in the log run. Neither is part of a timed round, and a run resumed from stored
        samples times no round at all, so the twin is compiled here when no round compiled it."""
        self.binary(program)
        times = {"rust": self.rust_compile.get(program["id"]), "ry_typed": self.typed_compile.get(program["id"])}
        return {key: round(seconds, 6) for key, seconds in times.items() if seconds is not None}


def rustc():
    found = shutil.which("rustc")
    if found is None:
        sys.exit(f"error: the {RUST_SUITE} suite compiles its Rust programs with rustc, which is not on the PATH")
    return found


class PublicRunner:
    """Public programs: one run per process from the program's own directory, which prints
    `<name> <checksum> <seconds>`; the time includes byte compilation and tier-up. The typed
    version runs from a copy in a strict-mode project, still from the program's own directory,
    because some programs read sibling files by relative path. A typed version shared by
    several variants prints the name of the file it comes from."""

    def __init__(self, suite, args):
        self.network = []
        self.work = pathlib.Path(tempfile.mkdtemp(prefix="ry-bench-public-"))
        (self.work / "ry.toml").write_text(STRICT_CONFIG)

    def run(self, program, engine):
        source = root / program["source"]
        script = source.name
        stem = source.name.removesuffix(".R")
        if engine.typed:
            script = self.work / f"{program['id']}.R"
            script.write_text((root / program["typed_source"]).read_text())
            stem = pathlib.Path(program["typed_source"]).name.removesuffix(".typed.R")
        # The program's clock starts after the compile of an engine that compiles ahead, so the
        # log supplies that time. Its lines are written before the clock starts.
        env = dict(engine.env, RY_TYPED_LOG="1") if engine.ahead else engine.env
        status, out, err, _, memory = spawn(engine.command + [str(script)], env, cwd=source.parent)
        lines = [line for line in out.splitlines() if line.startswith(stem + " ")]
        if status != 0 or not lines:
            return {"seconds": None, "reason": failure_reason(out, err)}
        _, checksum, seconds = lines[-1].split()
        seconds = float(seconds)
        if engine.ahead:
            compiled = COMPILED_AHEAD.search(err)
            if compiled is None:
                return {"seconds": None, "reason": "the run does not report what it compiled ahead of time"}
            seconds += int(compiled["microseconds"]) / 1e6
        return {"seconds": seconds, "checksum": checksum, **memory_fields(memory)}

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
                 if line.startswith("typed: ") and not COMPILED.search(line) and AWAITING not in line]
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
        # A package whose DESCRIPTION asks for parallel tests would otherwise run them in a
        # testthat worker process, outside the measured process and with its own start-up.
        env = dict(engine.env, TESTTHAT_PARALLEL="false", TESTTHAT_CPUS="1", _R_CHECK_FORCE_SUGGESTS_="false")
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


def codebase_library():
    """The benchmark dependency closure that `just corpus` installs, which the code bases import."""
    return pathlib.Path(os.environ.get("RY_CORPUS_DIR", root / ".corpus")) / "library"


class CodebaseRunner:
    """The benchmark code bases: `oracle bench` runs one code base's test blocks on one engine and
    reports each block's time, and the sum of those is the program's time."""

    def __init__(self, suite, args):
        self.network = []
        self.oracle = root / "target/release/oracle"
        if not self.oracle.exists():
            sys.exit("error: the codebases suite runs through the oracle; build it with "
                     "`cargo build --release -p oracle`")
        if not codebase_library().is_dir():
            sys.exit("error: no dependency closure; run `just corpus` first")
        self.ry = args.ry
        self.environment = codebase_environment(args.rscript)
        self.work = pathlib.Path(tempfile.mkdtemp(prefix="ry-bench-codebases-"))
        generator = load_suite(suite)["generator"]
        subprocess.run([sys.executable, "-I", str(here / "tooling/generate.py"), "--out", str(self.work),
                        "--lines", str(generator["lines"]), "--files", str(generator["files"]),
                        "--seed", str(generator["seed"]), "--ry", self.ry],
                       check=True, stdout=subprocess.DEVNULL)

    def run(self, program, engine):
        return run_codebase(self.oracle, self.ry, self.work / program["id"], engine,
                            dict(engine.env, **self.environment), self.work / f"{program['id']}-{engine.key}.json")

    def close(self):
        subprocess.run(["rm", "-rf", str(self.work)], check=True)


def codebase_environment(rscript):
    """What `oracle bench` needs beside an engine's environment: GNU R's Rscript, and the loader
    path to GNU R's libraries, because `ry load --runtime gnu-r` loads libR.so itself, which finds
    libRblas.so only through the loader path that R's own launcher script would set."""
    lib = subprocess.run([rscript, "--vanilla", "-e", "cat(R.home('lib'))"],
                         capture_output=True, text=True, check=True).stdout.strip()
    return {"ORACLE_R_RSCRIPT": rscript, "LD_LIBRARY_PATH": lib}


def run_codebase(oracle, ry, directory, engine, env, report):
    """One run of a generated code base's test suite on one engine through `oracle bench`, which
    loads the package with `ry load` and times each test block; the time is the sum of the blocks'.
    tooling/hot.py runs the same command to capture the code base's package entries."""
    side = "--gnu-r-only" if engine.key == "gnu_r" else "--ry-only"
    command = [str(oracle), "bench", "--ry", ry, side, "--json", str(report), str(directory)]
    status, out, err, seconds, _ = spawn(command, env, timeout=CODEBASE_TIMEOUT, check_images=False)
    # The oracle exits 1 when the engines' tests do not all match, which a single-engine run
    # never satisfies, so the report decides.
    if status not in (0, 1) or not report.exists():
        return {"seconds": None, "reason": failure_reason(out, err)}
    summary = json.loads(report.read_text())[0]
    report.unlink()
    prefix = "gnu_r" if engine.key == "gnu_r" else "ry"
    suite_seconds = summary[f"{prefix}_suite_seconds"]
    if suite_seconds is None:
        return {"seconds": None, "reason": f"the run never finished ({summary['ry_exit']})"}
    return {"seconds": round(suite_seconds, 3),
            "checksum": f"{summary['tests']} tests, {summary[prefix + '_passing']} passing",
            "wall": round(seconds, 1),
            "blocks": [comparison[f"{prefix}_seconds"] for comparison in summary["comparisons"]]}


class HotRunner:
    """The hot suite: hot/harness.R, after hot/replay.R, runs the round's programs in one process
    per engine, so packages load once, and prints a line per program with the checksums of its
    samples' last results and the times of its batches (`parse_hot_line`). A program's time in a
    round is the median of its batches in that process."""

    batched = True

    def __init__(self, suite, args, directory=None):
        self.network = []
        # The harness runs in a directory of its own, so a relative suite path would not resolve.
        self.directory = (directory or here / suite).resolve()
        self.work = pathlib.Path(tempfile.mkdtemp(prefix=f"ry-bench-{suite}-"))
        self.script = self.work / "harness.R"
        self.script.write_text((here / HOT_SUITE / "replay.R").read_text() + "\n"
                               + (here / HOT_SUITE / "harness.R").read_text())
        self.session = ",".join(tomllib.loads((self.directory / "suite.toml").read_text())["session"])

    def run(self, program, engine):
        return self.run_batch([program], engine)[program["id"]]

    def run_batch(self, programs, engine, mode="time", env=None, wrapper=(), timeout=HOT_TIMEOUT):
        """Each program's result from one harness process on `engine`. A process that ends early
        keeps the lines it printed, but one that timed out or started without its images counts
        for nothing."""
        table = self.work / "programs.tsv"
        table.write_text(hot_program_table(programs))
        variables = dict(engine.env, RY_HOT_SUITE=str(self.directory), RY_HOT_PROGRAMS=str(table), RY_HOT_MODE=mode,
                         RY_HOT_SESSION=self.session, **(env or {}))
        # No memory sampling: the suite reports no peaks, and reading the memory map of a process
        # with a hundred packages loaded every 20 ms would slow the process it measures.
        status, out, err, _, _ = spawn([*wrapper, *engine.command, self.script.name], variables, cwd=self.work,
                                       timeout=timeout, sample=False)
        names = {program["id"] for program in programs}
        reported = {} if status is None else dict(
            found for line in out.splitlines() if (found := parse_hot_line(line)) and found[0] in names)
        missing = {"seconds": None, "reason": failure_reason(out, err) if status != 0 else "the harness printed no line for it"}
        return {program["id"]: reported.get(program["id"], missing) for program in programs}

    def close(self):
        shutil.rmtree(self.work, ignore_errors=True)


def hot_program_table(programs):
    """The program table hot/harness.R reads: a header, then per program its id and
    comma-separated lists of its sample files, iterations, rng, threw and fresh flags and namespaces."""
    def flags(program, key):
        return ",".join("1" if flag else "0" for flag in program[key])
    lines = ["id\tsamples\titerations\trng\tthrew\tfresh\tnamespaces"]
    for program in programs:
        lines.append("\t".join([program["id"], ",".join(program["samples"]), ",".join(map(str, program["iterations"])),
                                flags(program, "rng"), flags(program, "threw"), flags(program, "fresh"),
                                ",".join(program["namespaces"])]))
    return "\n".join(lines) + "\n"


def parse_hot_line(line):
    """A hot/harness.R line as `(id, result)`, or None for any other line. The line is `<id>
    <checksums> <min batch seconds> <median batch seconds> <calls per batch>`, with `NA` times in
    instructions mode, which times nothing, or `<id> failed <reason>`."""
    fields = line.split(" ")
    if len(fields) >= 3 and fields[1] == "failed":
        return fields[0], {"seconds": None, "reason": " ".join(fields[2:])}
    if len(fields) != 5:
        return None
    name, checksum, least, median, calls = fields
    try:
        result = {"checksum": checksum, "calls": int(calls)}
        if median != "NA":
            result.update(seconds=float(median), min=float(least))
    except ValueError:
        return None
    return name, result


def hot_summary(results, programs, table, state, args):
    """Adds what the hot suite records beside the common fields: each program's calls per batch,
    its share and weight in each code base, and per engine the predicted speed-up of each code base.

    A program's `share` of a code base is the share of the capture run's time (ry with the JIT off
    and the recorder on) measured in calls to its function. Its `weight` is its share of the code
    base's GNU R time among the suite's programs, which tooling/hot.py estimates from the capture
    when it generates the suite. The predicted speed-up of a code base is GNU R's time over ry's
    for the programs run in the proportions the code base runs them: with each program's paired
    speed-up r (GNU R / ry), 1 / sum(w / r), the weighted harmonic mean, over the programs that
    have a speed-up, whose summed weight `weight` records. Its interval is a bootstrap of that
    weighted harmonic mean (`predicted_interval`) that resamples each program's rounds, so it
    covers round-to-round noise only: not the uncertainty in the weights, nor which programs the
    suite selected."""
    rows = {row["id"]: row for row in results["programs"]}
    for program in programs:
        row = rows[program["id"]]
        calls = [run["calls"] for round_ in state["rounds"] for run in round_.get(program["id"], {}).values() if "calls" in run]
        if calls:
            row["calls"] = calls[-1]
        row["share"] = program.get("share", {})
        row["weight"] = program.get("weight", {})
    codebases = list(dict.fromkeys(codebase for program in programs for codebase in program.get("weight", {})))
    predicted = {}
    for engine in table[1:]:
        found = {}
        for codebase in codebases:
            weighted = [(program["weight"][codebase], round_ratios(state, program["id"], table, engine, "speedup"))
                        for program in programs
                        if program["weight"].get(codebase, 0) > 0 and not rows[program["id"]].get("failures")]
            weighted = [(weight, ratios) for weight, ratios in weighted if ratios]
            if weighted:
                point, low, high = predicted_interval(weighted, args.seed)
                found[codebase] = {"median": round(point, 4), "low": round(low, 4), "high": round(high, 4),
                                   "weight": round(sum(weight for weight, _ in weighted), 4), "programs": len(weighted)}
        if found:
            predicted[engine.key] = found
    results["predicted"] = predicted


def predicted_interval(weighted, seed, resamples=2000):
    """The weighted harmonic mean of the programs' median speed-ups, `weighted` being pairs of a
    weight and the program's per-round ratios, and a 95% bootstrap interval that resamples each
    program's rounds."""
    total = sum(weight for weight, _ in weighted)

    def predict(medians):
        return total / sum(weight / median for (weight, _), median in zip(weighted, medians))
    point = predict([statistics.median(ratios) for _, ratios in weighted])
    draw = random.Random(seed)
    values = sorted(predict([statistics.median(draw.choices(ratios, k=len(ratios))) for _, ratios in weighted])
                    for _ in range(resamples))
    return point, values[int(0.025 * resamples)], values[int(0.975 * resamples) - 1]


def hot_passes(runner, programs, table, args, results):
    """The hot suite's passes on ry beside the timed rounds: `--breakdown` and `--instructions`."""
    rows = {row["id"]: row for row in results["programs"]}
    ry = next(engine for engine in table if engine.key == "ry")
    if args.breakdown:
        path = runner.work / "regions.jsonl"
        path.unlink(missing_ok=True)
        measured = runner.run_batch(programs, ry, mode="regions",
                                    env={"RY_PROFILE_REGIONS": str(path), "RY_PROFILE_INTERVAL_US": "100"})
        regions = parse_regions(path.read_text()) if path.exists() else {}
        if not regions:
            print(f"warning: {args.ry} wrote no region profile, so it has no `.ry_bench_region`; no breakdown")
        for program in programs:
            name = program["id"]
            if name in regions:
                rows[name]["breakdown"] = regions[name]
            elif regions:
                print(f"warning: no region profile for {name}: {measured[name].get('reason', 'the profile has no such region')}")
        results["breakdown_weighting"], results["breakdown"] = weighted_breakdown(results["programs"])
    if args.instructions:
        found, functions = instructions_of(runner, programs, ry, functions=args.functions > 0)
        counts = {"ry": found}
        if functions:
            results["functions"] = weighted_functions(programs, found, functions, args.functions)
        for engine in table:
            if engine.key in ("baseline", "ry_nojit"):
                counts[engine.key], _ = instructions_of(runner, programs, engine)
        for key, measured in counts.items():
            for name, found in measured.items():
                rows[name].setdefault("instructions", {})[key] = found
        results["instructions"] = {key: weighted_instructions(programs, measured) for key, measured in counts.items()}
        if args.save_instructions:
            saved = {"ry": args.ry, "ry_commit": results["meta"].get("ry_commit"), "programs": counts["ry"]}
            pathlib.Path(args.save_instructions).write_text(json.dumps(saved, indent=2) + "\n")
            print(f"instruction counts in {args.save_instructions}")
        changes = {}
        if args.baseline_instructions:
            saved = json.loads(pathlib.Path(args.baseline_instructions).read_text())
            changes[args.baseline_instructions] = compare_instructions(programs, saved["programs"], counts["ry"])
        if "baseline" in counts:
            changes[args.baseline] = compare_instructions(programs, counts["baseline"], counts["ry"])
        if "ry_nojit" in counts:
            changes["the JIT off"] = compare_instructions(programs, counts["ry_nojit"], counts["ry"])
        if changes:
            results["instruction_changes"] = changes


def parse_regions(text):
    """The region profile ry writes under RY_PROFILE_REGIONS, one JSON object per line and region,
    as each region's seconds, profiler samples, share of samples per activity, and counters. The
    activity keys are whatever categories the runtime reports. The empty region is the time
    outside every region."""
    regions = {}
    for line in text.splitlines():
        if not line.strip():
            continue
        region = json.loads(line)
        if not region.get("region"):
            continue
        activities = region.get("activities", {})
        total = sum(activities.values())
        regions[region["region"]] = {
            "seconds": region.get("seconds"), "samples": region.get("samples"),
            "activities": {key: round(count / total, 4) for key, count in sorted(activities.items(), key=lambda item: -item[1])}
            if total else {},
            "counters": region.get("counters", {}),
        }
    return regions


def weighted_breakdown(rows):
    """Per code base, the share of ry's time in each activity, and which time weighs the programs.
    Each program's breakdown is weighted by its share of the code base's ry time, which is its GNU
    R weight over its speed-up. Without a speed-up for every program with a breakdown (a run with
    `--rounds 0` times nothing) it is weighted by the GNU R weight alone, for every program, and
    the weighting returned says `GNU R time` in place of `ry time`."""
    found = {}
    profiled = [row for row in rows if row.get("breakdown")]
    timed = all(row.get("ratio", {}).get("ry", {}).get("median") for row in profiled)
    weighting = "ry time" if timed else "GNU R time"
    codebases = list(dict.fromkeys(codebase for row in rows for codebase in row.get("weight", {})))
    for codebase in codebases:
        weighted = [(row["weight"][codebase] / (row["ratio"]["ry"]["median"] if timed else 1.0), row["breakdown"]["activities"])
                    for row in profiled if row["weight"].get(codebase, 0) > 0]
        total = sum(weight for weight, _ in weighted)
        if not total:
            continue
        shares = {}
        for weight, activities in weighted:
            for key, share in activities.items():
                shares[key] = shares.get(key, 0.0) + weight / total * share
        found[codebase] = {key: round(share, 4) for key, share in sorted(shares.items(), key=lambda item: -item[1])}
    return weighting, found


def instructions_of(runner, programs, engine, functions=False):
    """Each program's instructions in one batch on `engine`, counted by callgrind in the region
    the harness opens around it, which dumps each time the region ends; a region entered more
    than once has its dumps summed. With `functions`, also each program's instructions per
    function (`parse_callgrind_functions`); otherwise that is empty."""
    if shutil.which("valgrind") is None:
        sys.exit("error: --instructions runs ry under callgrind, and valgrind is not on the PATH")
    prefix = runner.work / f"callgrind.{engine.key}"
    for old in runner.work.glob(f"{prefix.name}*"):
        old.unlink()
    wrapper = ["valgrind", "--tool=callgrind", "--instr-atstart=no", "--smc-check=all-non-file",
               f"--callgrind-out-file={prefix}"]
    measured = runner.run_batch(programs, engine, mode="instructions", wrapper=wrapper, timeout=CODEBASE_TIMEOUT)
    dumps, by_function = {}, {}
    for path in sorted(runner.work.glob(f"{prefix.name}.*")):
        text = path.read_text(errors="replace")
        if found := parse_callgrind_dump(text):
            dumps[found[0]] = dumps.get(found[0], 0) + found[1]
            if functions:
                summed = by_function.setdefault(found[0], {})
                for key, count in parse_callgrind_functions(text).items():
                    summed[key] = summed.get(key, 0) + count
    counts = {}
    for program in programs:
        name, result = program["id"], measured[program["id"]]
        if name in dumps and result.get("calls"):
            counts[name] = {"batch": dumps[name], "calls": result["calls"], "per_call": round(dumps[name] / result["calls"], 1)}
        else:
            print(f"warning: no instruction count for {name} on {engine.label}: "
                  f"{result.get('reason') or 'callgrind wrote no dump for its region'}")
    return counts, by_function


def parse_callgrind_dump(text):
    """The region and instruction count of one callgrind dump, or None for a dump no region
    triggered (the one at program exit). A dump the region call triggers names the region in its
    `desc: Trigger: Client Request: <name>` line, and its `totals:` line starts with the count of
    the first event, which is instructions unless other events are asked for. The `summary:` line
    is no use: it reads 0, because the region stops instrumentation before it dumps."""
    trigger = re.search(r"^desc: Trigger: Client Request: (.*)$", text, re.M)
    total = re.search(r"^totals: (\d+)", text, re.M)
    if trigger is None or total is None or not trigger[1].strip():
        return None
    return trigger[1].strip(), int(total[1])


def parse_callgrind_functions(text):
    """The instructions each function ran itself in one callgrind dump, as `{(object, function):
    count}`, with a function callgrind has no symbol for folded into `(no symbols)` of its object.

    A dump names an object or a function in full once, as `(id) name`, and by `(id)` afterwards,
    and `ob`/`cob` share one id table as `fn`/`cfn` do. A cost line starts with one position per
    entry of the `positions:` header and then the instruction count. The cost line after a
    `calls=` line is what the callee ran, which the callee's own lines already hold."""
    names = {"ob": {}, "fn": {}}
    current = {"ob": "???", "fn": None}
    columns = 1
    costs = {}
    after_call = False
    for line in text.splitlines():
        if not line:
            continue
        if line[0].isdigit() or line[0] in "+-*":
            if after_call:
                after_call = False
            elif current["fn"] is not None:
                key = (current["ob"], current["fn"])
                costs[key] = costs.get(key, 0) + int(line.split()[columns])
            continue
        key, separator, value = line.partition("=")
        if key == "positions: instr line":
            columns = 2
        if not separator:
            continue
        if key == "calls":
            after_call = True
        elif key in ("ob", "cob", "fn", "cfn"):
            table = names[key[-2:]]
            name = value
            if compressed := re.match(r"\((\d+)\)\s*(.*)$", value):
                name = table.setdefault(compressed[1], compressed[2]) if compressed[2] else table.get(compressed[1], value)
            if key in ("ob", "fn"):
                current[key] = name
    folded = {}
    for (shared_object, function), count in costs.items():
        label = os.path.basename(shared_object) or shared_object
        # Callgrind numbers the levels of a recursion as `name'2`, `name'3`.
        function = re.sub(r"'\d+$", "", function)
        function = "(no symbols)" if re.fullmatch(r"0x[0-9a-fA-F]+", function) else re.sub(r"::h[0-9a-f]{16}$", "", function)
        folded[label, function] = folded.get((label, function), 0) + count
    return folded


def weighted_functions(programs, counts, by_function, limit):
    """The `limit` functions that hold the most of the code bases' instructions, with each one's
    share per code base: a program's instructions in a function are scaled like the program's
    total in `weighted_instructions`, and a share is over the sum of all functions. A function's
    rank is its largest share in any code base."""
    codebases = list(dict.fromkeys(codebase for program in programs for codebase in program.get("weight", {})))
    totals = {codebase: {} for codebase in codebases}
    for program in programs:
        name = program["id"]
        if name not in counts or name not in by_function:
            continue
        scale = 1 / (counts[name]["calls"] * program["gnu_seconds_per_call"])
        for codebase in codebases:
            weight = program["weight"].get(codebase, 0)
            for key, count in by_function[name].items():
                totals[codebase][key] = totals[codebase].get(key, 0) + weight * scale * count
    sums = {codebase: sum(found.values()) for codebase, found in totals.items()}
    shares = {}
    for codebase, found in totals.items():
        for key, value in found.items():
            if sums[codebase]:
                shares.setdefault(key, {})[codebase] = value / sums[codebase]
    ranked = sorted(shares.items(), key=lambda item: (-max(item[1].values()), item[0]))[:limit]
    return [{"object": key[0], "function": key[1], "share": {codebase: round(share, 5) for codebase, share in found.items()}}
            for key, found in ranked]


def weighted_instructions(programs, counts):
    """Per code base, ry's instructions per second of GNU R work: each program's instructions per
    call over GNU R's seconds per call, averaged with the program's weight over the programs
    counted. The GNU R time is the one fixed when the suite was generated, so the figure moves only
    with the build under test."""
    found = {}
    codebases = list(dict.fromkeys(codebase for program in programs for codebase in program.get("weight", {})))
    for codebase in codebases:
        weighted = [(program["weight"][codebase], counts[program["id"]]["per_call"] / program["gnu_seconds_per_call"])
                    for program in programs if program["id"] in counts and program["weight"].get(codebase, 0) > 0]
        total = sum(weight for weight, _ in weighted)
        if total:
            found[codebase] = round(sum(weight * rate for weight, rate in weighted) / total)
    return found


def compare_instructions(programs, old, new):
    """Each program's change in instructions per call from `old` to `new`, with its effect on each
    code base's weighted total (`weighted_instructions`) in percent, sorted by the largest effect,
    over the programs both count."""
    common = [program for program in programs if program["id"] in old and program["id"] in new]
    codebases = list(dict.fromkeys(codebase for program in common for codebase in program.get("weight", {})))
    totals = {codebase: sum(program["weight"].get(codebase, 0) * old[program["id"]]["per_call"] / program["gnu_seconds_per_call"]
                            for program in common) for codebase in codebases}
    changes = []
    for program in common:
        before, after = old[program["id"]]["per_call"], new[program["id"]]["per_call"]
        effect = {codebase: round(100 * program["weight"].get(codebase, 0) * (after - before) / program["gnu_seconds_per_call"]
                                  / totals[codebase], 3) for codebase in codebases if totals[codebase]}
        changes.append({"id": program["id"], "before": before, "after": after,
                        "change": round(100 * (after - before) / before, 3) if before else None, "effect": effect})
    changes.sort(key=lambda change: (-max((abs(value) for value in change["effect"].values()), default=0), change["id"]))
    return changes


def print_hot(results):
    """What the hot suite adds to the common table: the predicted speed-up of each code base, the
    breakdown and the instruction counts."""
    for key, found in results.get("predicted", {}).items():
        for codebase, value in found.items():
            print(f"\nPredicted {ratio_label(results, key)} for {codebase}: {value['median']:.2f}x "
                  f"(95% interval {value['low']:.2f} to {value['high']:.2f}; {value['programs']} programs "
                  f"carrying {value['weight']:.0%} of its weight)")
    rows = [row for row in results["programs"] if row.get("breakdown")]
    if rows:
        totals = {}
        for row in rows:
            for key, share in row["breakdown"]["activities"].items():
                totals[key] = totals.get(key, 0.0) + share
        activities = sorted(totals, key=lambda key: (-totals[key], key))
        print("\nWhere ry spends each program's time, in shares of its profiler samples\n")
        print("| program | samples | " + " | ".join(activities) + " |")
        print("| --- | ---: |" + " ---: |" * len(activities))
        for row in rows:
            shares = row["breakdown"]["activities"]
            print(f"| {row['display']} | {row['breakdown']['samples']} | "
                  + " | ".join(f"{shares[key]:.0%}" if key in shares else "" for key in activities) + " |")
        for codebase, shares in results.get("breakdown", {}).items():
            weighting = results.get("breakdown_weighting", "ry time")
            print(f"| {codebase}, weighted by {weighting} | | " + " | ".join(f"{shares[key]:.0%}" if key in shares else "" for key in activities) + " |")
    counted = [row for row in results["programs"] if row.get("instructions")]
    if counted:
        keys = list(dict.fromkeys(key for row in counted for key in row["instructions"]))
        print("\nInstructions under callgrind, in one batch and per call\n")
        print("| program | calls | " + " | ".join(f"{results['engines'][key]}, batch | per call" for key in keys) + " |")
        print("| --- | ---: |" + " ---: | ---: |" * len(keys))
        for row in counted:
            cells = []
            for key in keys:
                found = row["instructions"].get(key)
                cells += [f"{found['batch']:,}", f"{found['per_call']:,.0f}"] if found else ["", ""]
            calls = next(iter(row["instructions"].values()))["calls"]
            print(f"| {row['display']} | {calls} | " + " | ".join(cells) + " |")
        for key, totals in results["instructions"].items():
            for codebase, total in totals.items():
                print(f"Weighted, {codebase}, {results['engines'][key]}: {total:,} instructions per second of GNU R work")
    if results.get("functions"):
        codebases = list(dict.fromkeys(codebase for row in results["functions"] for codebase in row["share"]))
        print("\nFunctions by their own share of each code base's instructions\n")
        print("| function | object | " + " | ".join(codebases) + " |")
        print("| --- | --- |" + " ---: |" * len(codebases))
        for row in results["functions"]:
            print(f"| {row['function']} | {row['object']} | "
                  + " | ".join(f"{row['share'].get(codebase, 0):.2%}" for codebase in codebases) + " |")
    for against, changes in results.get("instruction_changes", {}).items():
        print(f"\nInstructions per call against {against}, by effect on each code base's weighted total\n")
        codebases = list(dict.fromkeys(codebase for change in changes for codebase in change["effect"]))
        print("| program | before | after | change | " + " | ".join(f"effect, {codebase}" for codebase in codebases) + " |")
        print("| --- | ---: | ---: | ---: |" + " ---: |" * len(codebases))
        for change in changes:
            percent = "" if change["change"] is None else f"{change['change']:+.2f}%"
            print(f"| {change['id']} | {change['before']:,.0f} | {change['after']:,.0f} | {percent} | "
                  + " | ".join(f"{change['effect'][codebase]:+.3f}%" if codebase in change["effect"] else ""
                               for codebase in codebases) + " |")


def per_block(state, name, table, engine):
    """GNU R's median time over the engine's, geometrically averaged over the blocks GNU R runs in
    `PER_TEST_FLOOR` or more, or None when the runs disagree on the blocks."""
    def medians(key):
        runs = [round_[name][key]["blocks"] for round_ in state["rounds"] if key in round_.get(name, {})]
        if not runs or len({len(blocks) for blocks in runs}) > 1:
            return None
        return [statistics.median(times) for times in zip(*runs)]
    reference, measured = medians(table[0].key), medians(engine.key)
    if reference is None or measured is None or len(reference) != len(measured):
        return None
    counted = [(base, time) for base, time in zip(reference, measured) if base >= PER_TEST_FLOOR and time > 0]
    if not counted:
        return None
    return {"blocks": len(counted), "of": len(reference),
            "geomean": round(geomean([base / time for base, time in counted]), 4)}


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


def round_ratios(state, program, table, engine, kind):
    """An engine's ratio to the baseline, the first engine of the table, in each round where both
    ran: the baseline's time on top for a speed-up, the engine's for a slowdown."""
    baseline = table[0].key
    if kind == "slowdown":
        return paired(state, program, engine.key, baseline)
    return paired(state, program, baseline, engine.key)


def wide_interval(state, program, table, kind):
    for engine in table[1:]:
        ratios = round_ratios(state, program, table, engine, kind)
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
    baseline, kind, floor = table[0], ratio_kind(suite), short_floor(suite)
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
            if engine is baseline:
                reference = checksums[0]
            elif reference is not None and not same_checksum(checksums[0], reference):
                if suite == "packages":
                    row["flags"].append({"kind": "tests_differ", "engine": engine.key,
                                         "reason": f"GNU R: {reference}; {engine.label}: {checksums[0]}"})
                else:
                    failures.append({"engine": engine.key,
                                     "reason": f"checksum {checksums[0]}, {baseline.label} printed {reference}"})
            median = statistics.median(run["seconds"] for run in runs)
            row["seconds"][engine.key] = round(median, 6)
            if all("first" in run for run in runs):
                row.setdefault("first_seconds", {})[engine.key] = round(statistics.median(run["first"] for run in runs), 6)
            for field in MEMORY_PARTS.values():
                sizes = [run[field] for run in runs if field in run]
                if sizes:
                    row[field][engine.key] = max(sizes)
            if median < floor:
                row["flags"].append({"kind": "too_short", "engine": engine.key})
        if failures:
            row["failures"] = failures
        short = row["seconds"].get(baseline.key, floor) < floor
        for engine in engines_here[1:]:
            ratios = round_ratios(state, name, table, engine, kind)
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
        if suite == "codebases" and not failures:
            row["per_block"] = {engine.key: found for engine in table[1:]
                                if (found := per_block(state, name, table, engine))}
        typed = next((engine for engine in table if engine.typed), None)
        if typed is not None and "typed_source" in program and not failures:
            reason = runner.typed_refusal(program, typed)
            if reason:
                row["flags"].append({"kind": "typed_refused", "engine": typed.key, "reason": reason})
        if suite == RUST_SUITE and not failures:
            row["compile_seconds"] = runner.compile_seconds(program)
        rows.append(row)
    def geomeans(included):
        means = {}
        for key, per_program in included.items():
            if per_program:
                point, low, high = geomean_interval(per_program, args.seed)
                means[key] = {"median": round(point, 4), "low": round(low, 4), "high": round(high, 4),
                              "programs": len(per_program)}
        return means
    def worst_programs():
        """Each engine's slowest program by median ratio, among those the geometric mean counts."""
        found = {}
        for engine in table[1:]:
            counted = [(row["ratio"][engine.key]["median"], row["id"]) for row in rows
                       if engine.key in row["ratio"] and not any(
                           flag["kind"] == "too_short" and flag["engine"] in (table[0].key, engine.key)
                           for flag in row["flags"])]
            if counted:
                ratio, name = min(counted) if kind == "speedup" else max(counted)
                found[engine.key] = {"id": name, "ratio": ratio}
        return found

    results = {
        "schema": SCHEMA, "suite": suite, "name": manifest["name"],
        "meta": dict(meta, too_short_seconds=floor, rounds=args.rounds, extra_rounds=args.extra_rounds, seed=args.seed),
        "engines": {engine.key: engine.label for engine in table},
        "baseline": baseline.key, "ratio_kind": kind,
        "programs": rows, "geomean": geomeans(included), "worst": worst_programs(),
    }
    if pairs:
        results["paired_geomean"] = geomeans(paired_included)
    if suite == HOT_SUITE:
        hot_summary(results, programs, table, state, args)
    return results


def ratio_label(results, key):
    """What a ratio divides, in the order it divides it: `GNU R / ry` or `ry typed / Rust`."""
    labels = [results["engines"][results["baseline"]], results["engines"][key]]
    return " / ".join(reversed(labels) if results["ratio_kind"] == "slowdown" else labels)


def print_table(results, table):
    keys = [engine.key for engine in table if engine.key in results["engines"]]
    pairs = [key for key in keys if key in results.get("paired_geomean", {}) or
             any(key in row.get("paired", {}) for row in results["programs"])]
    label = results["engines"]
    print("| program | " + " | ".join(label[key] for key in keys) + " | "
          + " | ".join(ratio_label(results, key) for key in keys[1:])
          + "".join(f" | {label['baseline' + key.removeprefix('ry')]} / {label[key]}" for key in pairs)
          + " | flags |")
    print("| --- |" + " ---: |" * (2 * len(keys) - 1 + len(pairs)) + " --- |")
    # One unit per table: milliseconds where the suite times programs that finish in a few.
    unit = (lambda seconds: f"{seconds * 1000:.3f} ms") if results["meta"]["too_short_seconds"] < 0.001 else duration
    for row in results["programs"]:
        cells = [unit(row["seconds"][key]) if key in row["seconds"] else "" for key in keys]
        for ratio in [row["ratio"].get(key) for key in keys[1:]] + [row.get("paired", {}).get(key) for key in pairs]:
            cells.append(f"{ratio['median']:.2f}x ({ratio['low']:.2f} to {ratio['high']:.2f})" if ratio else "")
        flags = ", ".join(f"{flag['kind']} ({flag['engine']})" for flag in row["flags"])
        if row.get("failures"):
            flags = ", ".join(["FAILED"] + ([flags] if flags else []))
        print(f"| {row['display']} | " + " | ".join(cells) + f" | {flags} |")
    # The hot suite runs every program in one process, so no program has a peak of its own.
    if any(row.get(field) for row in results["programs"] for field in MEMORY_PARTS.values()):
        print("\nPeak memory in MiB: whole process / anonymous / start-up image / libR.so resident\n")
        print("| program | " + " | ".join(label[key] for key in keys) + " |")
        print("| --- |" + " ---: |" * len(keys))
        for row in results["programs"]:
            cells = [" / ".join(f"{row[field][key]:.1f}" if key in row.get(field, {}) else "-"
                                for field in MEMORY_PARTS.values()) for key in keys]
            print(f"| {row['display']} | " + " | ".join(cells) + " |")
    for key, mean in results["geomean"].items():
        print(f"\nGeometric mean of {ratio_label(results, key)} over {mean['programs']} programs: "
              f"{mean['median']:.2f}x (95% interval {mean['low']:.2f} to {mean['high']:.2f})")
    for key, mean in results.get("paired_geomean", {}).items():
        print(f"\nGeometric mean of {label['baseline' + key.removeprefix('ry')]} / {label[key]} over "
              f"{mean['programs']} programs: {mean['median']:.2f}x "
              f"(95% interval {mean['low']:.2f} to {mean['high']:.2f}; above 1 means {label[key]} is faster)")
    for build, size in results["meta"].get("libr_text_bytes", {}).items():
        print(f"\nlibR.so .text of {build}: {size / 1024 / 1024:.2f} MiB ({size} bytes)")
    first = [row for row in results["programs"] if row.get("first_seconds")]
    if first:
        print("\nFirst run of the program in its process, which holds what compiles at a first call\n")
        print("| program | " + " | ".join(label[key] for key in keys) + " |")
        print("| --- |" + " ---: |" * len(keys))
        for row in first:
            cells = [duration(row["first_seconds"][key]) if key in row["first_seconds"] else "" for key in keys]
            print(f"| {row['display']} | " + " | ".join(cells) + " |")
    compiled = [row for row in results["programs"] if row.get("compile_seconds")]
    # Only the engines whose compile time was measured get a column: a `--baseline` build has none.
    timed = [key for key in keys if any(key in row["compile_seconds"] for row in compiled)]
    if timed:
        print("\n| program | " + " | ".join(f"{results['engines'][key]}, compile" for key in timed) + " |")
        print("| --- |" + " ---: |" * len(timed))
        for row in compiled:
            print(f"| {row['display']} | " + " | ".join(compile_time(row["compile_seconds"].get(key)) for key in timed) + " |")
        totals = [sum(row["compile_seconds"].get(key, 0) for row in compiled) for key in timed]
        print("| total | " + " | ".join(compile_time(total) for total in totals) + " |")


def compile_time(seconds):
    return "" if seconds is None else f"{seconds * 1000:.1f} ms"


# Metadata.

def unreleased_build(binary):
    """The version of a ry that was not built with `--release`, or None for a release build.

    Any other profile names itself in `ry --version` (`ry 0.4.3-beta (iterate build)`): a debug build
    is many times slower, and the iterate profile skips the cross-crate inlining LTO does, so a timing
    of either would pass for a release number it is not. A run takes such a build only with
    `--any-build`.
    """
    try:
        version = subprocess.run([binary, "--version"], capture_output=True, text=True, timeout=60).stdout.strip()
    except (OSError, subprocess.TimeoutExpired) as error:
        sys.exit(f"error: cannot run {binary}: {error}")
    return version if re.search(r"\b[\w.-]+ build\b", version) else None


def unpack_release(archive):
    """Unpacks a release archive into a private directory and returns its `ry` with what results.json
    records about it: the tag, the archive's file name and sha256, and whether it is a nightly.

    The cache directory moves into that private directory too (XDG_CACHE_HOME), so the archive's
    `ry` extracts the runtime and base R it embeds afresh and reads no other build's from the user's
    cache, and RY_LIBR, RY_BASE_R and R_HOME are cleared so they cannot point it at another build.
    """
    try:
        digest = hashlib.sha256(archive.read_bytes()).hexdigest()
    except OSError as error:
        sys.exit(f"error: cannot read the release archive {archive}: {error}")
    scratch = pathlib.Path(tempfile.mkdtemp(prefix="ry-bench-release-"))
    atexit.register(shutil.rmtree, scratch, ignore_errors=True)
    try:
        with tarfile.open(archive) as tar:
            tar.extractall(scratch / "archive", filter="data")
    except (OSError, tarfile.TarError) as error:
        sys.exit(f"error: cannot unpack the release archive {archive}: {error}")
    ry = scratch / "archive" / "ry"
    if not ry.is_file():
        sys.exit(f"error: {archive} holds no `ry` at its top level, so it is not a Linux or macOS release archive")
    os.environ["XDG_CACHE_HOME"] = str(scratch / "cache")
    for variable in ("RY_LIBR", "RY_BASE_R", "R_HOME"):
        os.environ.pop(variable, None)
    try:
        version = subprocess.run([ry, "--version"], capture_output=True, text=True, timeout=60).stdout.strip()
    except (OSError, subprocess.TimeoutExpired) as error:
        sys.exit(f"error: cannot run the release archive's ry: {error}")
    tag, nightly = release_tag(version)
    if tag is None:
        sys.exit(f"error: the release archive's ry names no release (`ry --version` printed {version!r})")
    return str(ry), {"tag": tag, "nightly": nightly, "archive": archive.name, "sha256": digest}


def release_tag(version):
    """The tag `ry --version` names and whether it is a nightly: `ry 0.4.4-beta` is release tag
    `0.4.4-beta`, and `ry 0.4.3-beta (nightly.20261110.1955.a15ce0)` is the nightly of that name.
    A local build names its profile instead (`(iterate build)`), which is no tag."""
    match = re.fullmatch(r"ry (\S+)(?: \((nightly\.[\w.]+)\))?", version)
    if match is None:
        return None, False
    return (match[2] or match[1]), match[2] is not None


def runtime_identity(library):
    """The identity of the runtime library `library`, which names every image it writes."""
    import ctypes
    function = ctypes.CDLL(str(library), mode=os.RTLD_LOCAL).ry_runtime_id
    function.restype = ctypes.c_char_p
    return function().decode()


def metadata(args, release):
    cpu = platform.processor() or platform.machine()
    try:
        for line in pathlib.Path("/proc/cpuinfo").read_text().splitlines():
            if line.startswith("model name"):
                cpu = line.split(":", 1)[1].strip()
                break
    except OSError:
        pass
    return {
        "date": time.strftime("%Y-%m-%d"),
        "ry_commit": release_commit(release["tag"]) if release else commit(args.ry),
        "release": None if release is None else {key: release[key] for key in ("tag", "archive", "sha256")},
        "libr_text_bytes": {build: size for build, binary in (("ry", args.ry), ("baseline", args.baseline))
                            if binary and (runtime := runtime_of(binary)) and (size := text_size(runtime)) is not None},
        "machine": {"cpu": cpu, "cores": os.cpu_count(), "kernel": f"{platform.system()} {platform.release()}"},
        "command": "tools/bench/bench.py " + " ".join(shlex.quote(argument) for argument in sys.argv[1:]),
    }


def gnu_r_metadata(args):
    probe = subprocess.run([args.rscript, "--vanilla", "-e",
                            "cat(paste(R.version$major, R.version$minor, sep = '.'), '\\n', La_library(), '\\n', extSoftVersion()[['BLAS']], '\\n', sep = '')"],
                           capture_output=True, text=True, check=True).stdout.splitlines()
    version, lapack, blas = (probe + ["", "", ""])[:3]
    return {"version": version.strip(), "blas": (blas or lapack).strip()}


def rust_metadata():
    version = subprocess.run([rustc(), "--version"], capture_output=True, text=True, check=True).stdout.strip()
    return {"version": version.removeprefix("rustc "), "flags": " ".join(RUSTC_FLAGS)}


def release_commit(tag):
    """The commit a release tag names in this checkout, or None when the tag is not fetched."""
    found = subprocess.run(["git", "-C", str(root), "rev-parse", "--verify", "--quiet", f"refs/tags/{tag}^{{commit}}"],
                           capture_output=True, text=True)
    return found.stdout.strip() or None


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
