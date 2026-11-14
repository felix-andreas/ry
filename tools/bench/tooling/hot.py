#!/usr/bin/env python3
"""Generate the hot suite (tools/bench/hot/) from a capture of the benchmark code bases.

The hot suite replays the package calls that dominate the code bases' time on ry: each program is
one package entry (such as `stats::sd`) with a few captured calls of it, and hot/harness.R runs
`eval(call, env)` for them in a loop. The suite comes from three steps, which `just
bench-hot-capture DIR` runs in order on code bases that tooling/generate.py wrote to DIR:

  capture   runs each code base's tests on ry (`oracle bench`, as the codebases suite does, with the
            JIT off) while ry's recorder writes, per code base, the time spent in each package
            entry (entries.tsv, summary.tsv) and a few calls of each with their arguments
            (samples.tsv, samples/<n>.rds), then prints the shares and the top keys. It also
            lists the packages the code base imports (session.txt), which every replay loads.
  validate  replays every sample under GNU R (hot/validate.R), in a session with those packages
            loaded, and keeps the samples that replay as they ran without side effects outside
            the session's temporary directory, with GNU R's seconds per call, a batch size of
            about 20 ms and whether a call changes its own variables (validated.tsv,
            validated/<n>.rds).
  select    picks the keys that cover the most of each code base's time, up to `--programs`, and
            the most typical `--samples` valid samples of each, weighs them, and writes
            hot/suite.toml and hot/samples/. It reports what the suite loses: keys with no valid
            sample, and keys whose time is mostly calls the recorder could not capture.

A program's weight in a code base is its share of the code base's GNU R time among the suite's
programs, which bench.py needs to read the predicted speed-up of a code base as the weighted
harmonic mean of the programs' speed-ups. The capture measures ry's time, with the recorder on and
the JIT off, so GNU R's time in a key is estimated from the key's samples: the time the code base
spent in the key (`package_seconds`) times GNU R's seconds over the capture's seconds for the same
sampled calls (`gnu_time`). The
recorder samples a key's calls uniformly, and a sampled call ran under the same conditions as every
other call of the key, so whatever the capture run adds to the key's time cancels in that ratio.
Every step is deterministic for the same capture.

A fourth step checks that the suite predicts the code bases:

  track     times the suite and the code bases' tests on several ry configurations (other builds,
            or one build with other settings) and on GNU R, and reports per code base how the
            suite's predicted change of each configuration against the first compares with the
            measured change, with their correlation. The suite is representative when a change it
            shows is the change the code bases see.

Usage:
    python3 -I hot.py capture --codebases DIR --out DIR [--ry PATH] [--rscript PATH] [--only NAME,...]
    python3 -I hot.py validate --captures DIR [--rscript PATH]
    python3 -I hot.py select --captures DIR [--suite DIR] [--programs N] [--coverage F] [--samples N]
    python3 -I hot.py track --codebases DIR --out FILE.json [--rounds N] [--only NAME,...] [--rscript PATH]
                            NAME=RY[,VARIABLE=VALUE...] ...
"""

import argparse
import csv
import hashlib
import json
import math
import os
import pathlib
import shutil
import statistics
import subprocess
import sys
import tempfile
import tomllib

here = pathlib.Path(__file__).resolve().parent
sys.path.insert(0, str(here.parent))
import bench  # noqa: E402

CODEBASES = ("finbench", "dsbench", "biobench")
# The GNU R time a batch of one program's calls takes, split evenly between its samples.
BATCH_SECONDS = 0.02
TOP_KEYS = 25


def main():
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    steps = parser.add_subparsers(dest="step", required=True)
    capture = steps.add_parser("capture", help="run the code bases on ry and record their package entries")
    capture.add_argument("--codebases", required=True, type=pathlib.Path, help="where tooling/generate.py wrote them")
    capture.add_argument("--out", required=True, type=pathlib.Path)
    capture.add_argument("--only", default="", help="comma-separated code bases (default: all three)")
    validate = steps.add_parser("validate", help="replay and calibrate every captured sample under GNU R")
    validate.add_argument("--captures", required=True, type=pathlib.Path)
    select = steps.add_parser("select", help="write the suite from the validated samples")
    select.add_argument("--captures", required=True, type=pathlib.Path)
    select.add_argument("--suite", type=pathlib.Path, default=bench.here / bench.HOT_SUITE)
    select.add_argument("--programs", type=int, default=100, help="at most this many programs")
    select.add_argument("--coverage", type=float, default=0.9,
                        help="stop adding keys once every code base has this share of its package time")
    select.add_argument("--samples", type=int, default=3, help="samples per program")
    track = steps.add_parser("track", help="compare the suite's predicted changes with the code bases' measured ones")
    track.add_argument("--codebases", required=True, type=pathlib.Path, help="where tooling/generate.py wrote them")
    track.add_argument("--out", required=True, type=pathlib.Path,
                       help="the measurements, as JSON; one already in the file is not taken again")
    track.add_argument("--suite", type=pathlib.Path, default=bench.here / bench.HOT_SUITE)
    track.add_argument("--rounds", type=int, default=5, help="runs of the suite per configuration")
    track.add_argument("--only", default="", help="comma-separated code bases (default: all three)")
    track.add_argument("configurations", nargs="+", metavar="NAME=RY[,VARIABLE=VALUE...]",
                       help="a ry binary and the environment it runs with; the first is the reference")
    for step in (capture, validate, track):
        step.add_argument("--rscript", default=os.environ.get("ORACLE_R_RSCRIPT", "Rscript"), help="GNU R's Rscript")
    capture.add_argument("--ry", default=str(bench.root / "target/release/ry"), help="the ry build")
    args = parser.parse_args()
    {"capture": run_capture, "validate": run_validate, "select": run_select, "track": run_track}[args.step](args)


# Capture.

def run_capture(args):
    oracle = bench.root / "target/release/oracle"
    if not oracle.exists():
        sys.exit("error: the capture runs through the oracle; build it with `cargo build --release -p oracle`")
    names = [name for name in args.only.split(",") if name] or list(CODEBASES)
    library = dependency_library()
    base = {key: value for key, value in os.environ.items() if key not in ("RY_JIT", "RY_TYPED_LOG")}
    base.update(LC_ALL="C.UTF-8", RY_JIT="off", **bench.library_environment(args.rscript, library),
                **bench.codebase_environment(args.rscript))
    engine = bench.Engine("ry", "ry", None, base)
    args.out.mkdir(parents=True, exist_ok=True)
    work = pathlib.Path(tempfile.mkdtemp(prefix="ry-hot-capture-"))
    try:
        for name in names:
            directory = args.codebases / name
            if not directory.is_dir():
                sys.exit(f"error: no code base {directory}; write the code bases with tooling/generate.py")
            out = (args.out / name).resolve()
            shutil.rmtree(out, ignore_errors=True)
            out.mkdir(parents=True)
            print(f"capturing {name} ...", flush=True)
            env = dict(base, RY_CAPTURE_PACKAGE=name, RY_CAPTURE_DIR=str(out))
            result = bench.run_codebase(oracle, os.path.abspath(args.ry), directory.resolve(), engine, env,
                                        work / f"{name}.json")
            if result.get("seconds") is None:
                sys.exit(f"error: the capture run of {name} failed: {result['reason']}")
            print(f"{name}: {result['checksum']} in {result['seconds']:.1f} s of test blocks")
            (out / "session.txt").write_text("".join(f"{package}\n" for package in session_packages(directory)))
            print_capture(out)
    finally:
        shutil.rmtree(work, ignore_errors=True)


def session_packages(codebase):
    """The packages loaded in a session that ran the code base's tests: those its DESCRIPTION
    depends on or imports, and testthat, which its runner attaches."""
    fields, name = {}, None
    for line in (codebase / "DESCRIPTION").read_text().splitlines():
        if line[:1].isspace():
            fields[name] += line
        elif ":" in line:
            name, _, value = line.partition(":")
            fields[name] = value
    listed = ",".join(fields.get(field, "") for field in ("Depends", "Imports")).split(",")
    names = {entry.split("(")[0].strip() for entry in listed}
    return sorted((names | {"testthat"}) - {"", "R"}, key=str.lower)


def print_capture(directory):
    """The shares of the recorded run's time, and the keys that took the most of it."""
    summary = read_summary(directory)
    total = summary["total_seconds"] or 1.0
    print(f"  {total:.1f} s on ry: {summary['user_seconds'] / total:.0%} in the code base's own code, "
          f"{summary['covered_seconds'] / total:.0%} in package entries it captured, "
          f"{summary['uncapturable_seconds'] / total:.0%} in entries it could not capture, "
          f"{summary['outside_seconds'] / total:.0%} outside both; "
          f"{summary['boundary_calls']:.0f} calls into {summary['keys']:.0f} entries")
    entries = sorted(read_tsv(directory / "entries.tsv"),
                     key=lambda entry: (-(entry["seconds"] + entry["uncapturable_seconds"]), entry["package"], entry["name"]))
    print(f"  | entry | share | calls | samples | uncapturable share |")
    print(f"  | --- | ---: | ---: | ---: | ---: |")
    for entry in entries[:TOP_KEYS]:
        print(f"  | {entry['package']}::{entry['name']} | {entry['seconds'] / total:.2%} | {entry['calls']:.0f} | "
              f"{entry['samples']:.0f} | {entry['uncapturable_seconds'] / total:.2%} |")


# Validation.

def run_validate(args):
    directories = capture_directories(args.captures)
    script = pathlib.Path(tempfile.mkdtemp(prefix="ry-hot-validate-")) / "validate.R"
    script.write_text((bench.here / bench.HOT_SUITE / "replay.R").read_text() + "\n"
                      + (bench.here / bench.HOT_SUITE / "validate.R").read_text())
    # The validator watches every path under its temporary root for files a replay touches
    # outside the session's temporary directory, so the root is a directory of its own, and R's
    # user directories (where cachem, memoise and rappdirs write) lie under it too.
    watched = script.parent / "tmp"
    try:
        env = {key: value for key, value in os.environ.items() if not key.startswith("RY_")}
        env.update(LC_ALL="C.UTF-8", **bench.library_environment(args.rscript, dependency_library()))
        env.update(TMPDIR=str(watched), R_USER_CACHE_DIR=str(watched / "cache"), R_USER_DATA_DIR=str(watched / "data"),
                   R_USER_CONFIG_DIR=str(watched / "config"))
        for directory in directories:
            print(f"validating {directory.name} ...", flush=True)
            shutil.rmtree(watched, ignore_errors=True)
            watched.mkdir()
            subprocess.run([args.rscript, "--vanilla", str(script)], env=dict(env, RY_HOT_CAPTURE=str(directory.resolve())),
                           check=True)
            rows = read_tsv(directory / "validated.tsv")
            valid = [row for row in rows if row["valid"]]
            reasons = {}
            for row in rows:
                if not row["valid"]:
                    reason = row["reason"].split(":")[0]
                    reasons[reason] = reasons.get(reason, 0) + 1
            print(f"{directory.name}: {len(valid)} of {len(rows)} samples valid"
                  + "".join(f"; {count} {reason}" for reason, count in sorted(reasons.items(), key=lambda item: (-item[1], item[0]))))
    finally:
        shutil.rmtree(script.parent, ignore_errors=True)


# Selection.

def run_select(args):
    directories = capture_directories(args.captures)
    captures = {directory.name: load_capture(directory) for directory in directories}
    keys = choose_keys(captures, args.programs, args.coverage)
    programs = [build_program(key, captures, args.samples) for key in keys]
    assign_ids(programs)
    for codebase, capture in captures.items():
        weigh_programs(programs, codebase)
    write_suite(args.suite, programs, captures, {"programs": args.programs, "coverage": args.coverage,
                                                  "samples": args.samples})
    print_selection(programs, captures)


def load_capture(directory):
    """One code base's capture: its summary, its entries by key and its valid samples by key, each
    sample with the path of its compact copy."""
    summary = read_summary(directory)
    entries = {(entry["package"], entry["name"]): entry for entry in read_tsv(directory / "entries.tsv")}
    samples = {}
    validated = directory / "validated.tsv"
    if not validated.exists():
        sys.exit(f"error: {directory} has no validated.tsv; run `hot.py validate` first")
    captured = {row["file"]: row["seconds"] for row in read_tsv(directory / "samples.tsv")}
    for row in read_tsv(validated):
        if row["valid"]:
            row["seconds"] = captured[row["file"]]
            row["path"] = directory / "validated" / pathlib.PurePosixPath(row["file"]).name
            row["compact_bytes"] = row["path"].stat().st_size
            samples.setdefault((row["package"], row["name"]), []).append(row)
    session = (directory / "session.txt").read_text().split()
    return {"summary": summary, "entries": entries, "samples": samples, "session": session}


def package_seconds(entry):
    """The seconds a code base spent in an entry: the calls the recorder could capture, with
    everything below them, and the entry's own time in the calls it could not. A program stands for
    both, because the calls it replays run the same code as the ones the recorder had to pass."""
    return entry["seconds"] + entry["uncapturable_seconds"]


def choose_keys(captures, limit, coverage):
    """The keys to make programs of: those with a valid sample, by the summed share of each code
    base's package time they hold (`package_seconds`), until `limit` keys or until every code base
    has `coverage` of its package time in the chosen keys."""
    shares = {}
    for codebase, capture in captures.items():
        covered = sum(package_seconds(entry) for entry in capture["entries"].values()) or 1.0
        for key, entry in capture["entries"].items():
            if capture["samples"].get(key):
                shares.setdefault(key, {})[codebase] = package_seconds(entry) / covered
    ranked = sorted(shares, key=lambda key: (-sum(shares[key].values()), key))
    chosen, reached = [], {codebase: 0.0 for codebase in captures}
    for key in ranked:
        if len(chosen) >= limit or all(value >= coverage for value in reached.values()):
            break
        chosen.append(key)
        for codebase, share in shares[key].items():
            reached[codebase] += share
    return chosen


def build_program(key, captures, count):
    """A program for one key: its most typical valid samples from every code base, and per code
    base its share of the capture run's time and the GNU R time it stands for."""
    package, name = key
    calls = sum(capture["entries"][key]["calls"] for capture in captures.values() if key in capture["entries"])
    seconds = sum(capture["entries"][key]["seconds"] for capture in captures.values() if key in capture["entries"])
    candidates = [sample for capture in captures.values() for sample in capture["samples"].get(key, [])]
    samples = typical_samples(dedupe(candidates), seconds / calls if calls else 0.0, count)
    iterations = [max(1, round(BATCH_SECONDS / len(samples) / sample["gnu_seconds_per_call"])) for sample in samples]
    gnu = sum(times * sample["gnu_seconds_per_call"] for times, sample in zip(iterations, samples)) / sum(iterations)
    share = {codebase: package_seconds(capture["entries"][key]) / capture["summary"]["total_seconds"]
             for codebase, capture in captures.items() if key in capture["entries"] and capture["summary"]["total_seconds"]}
    return {"package": package, "name": name, "display": f"{package}::{name}", "chosen": samples,
            "iterations": iterations, "rng": [sample["rng"] for sample in samples],
            "threw": [sample["threw"] for sample in samples], "fresh": [sample["fresh"] for sample in samples],
            "namespaces": sorted({namespace for sample in samples for namespace in sample["namespaces"]}),
            "gnu_seconds_per_call": gnu, "share": share, "gnu_time": gnu_time(key, captures, candidates)}


def gnu_time(key, captures, candidates):
    """Per code base, the GNU R seconds the key's calls stand for: the seconds the code base spent
    in it (`package_seconds`) times GNU R's seconds over the capture's seconds for the same sampled calls. The ratio
    comes from the code base's own valid samples, and from every code base's when it has none or
    theirs took no measurable time."""
    def ratio(samples):
        captured = sum(sample["seconds"] for sample in samples)
        return sum(sample["gnu_seconds_per_call"] for sample in samples) / captured if captured > 0 else None
    pooled = ratio(candidates)
    found = {}
    for codebase, capture in captures.items():
        entry = capture["entries"].get(key)
        own = ratio(capture["samples"].get(key, []))
        factor = own if own is not None else pooled
        if entry and package_seconds(entry) > 0 and factor is not None:
            found[codebase] = package_seconds(entry) * factor
    return found


def dedupe(samples):
    """The samples with distinct compact copies, in a fixed order: two code bases often capture the
    same call of a shared helper."""
    seen, found = set(), []
    for sample in sorted(samples, key=lambda sample: (sample["compact_bytes"], str(sample["path"]))):
        digest = hashlib.sha256(sample["path"].read_bytes()).hexdigest()
        if digest not in seen:
            seen.add(digest)
            found.append(sample)
    return found


def typical_samples(samples, mean_seconds, count):
    """The `count` samples whose capture-run time is nearest the key's mean time per call, in 10%
    steps of log distance, and the smallest first within a step, so a program replays the calls a
    code base makes most like the average one and its suite stays small."""
    def distance(sample):
        if sample["seconds"] <= 0 or mean_seconds <= 0:
            return math.inf
        return math.floor(abs(math.log(sample["seconds"] / mean_seconds)) / math.log(1.1))
    return sorted(samples, key=lambda sample: (distance(sample), sample["compact_bytes"], str(sample["path"])))[:count]


# Operators and other names that are not words get a readable id.
OPERATOR_NAMES = {
    "[": "subset", "[[": "subset2", "$": "dollar", "@": "at", "[<-": "subassign", "[[<-": "subassign2",
    "$<-": "dollar-assign", "+": "plus", "-": "minus", "*": "times", "/": "divide", "^": "power", "%%": "modulo",
    "%/%": "intdiv", "%*%": "matmul", "%in%": "in", "%o%": "outer", "==": "eq", "!=": "ne", "<": "lt", ">": "gt",
    "<=": "le", ">=": "ge", "&": "and", "&&": "andand", "|": "or", "||": "oror", "!": "not", ":": "colon",
    "(": "paren", "{": "brace", "<-": "assign", "=": "equals", "~": "tilde", "%>%": "pipe", "%||%": "null-default",
}


def slug(package, name):
    """A program id: the package and the name in lower case, with every run of other characters
    a hyphen, and operators by their names."""
    text = OPERATOR_NAMES.get(name)
    if text is None:
        text = "".join(character if character.isalnum() else "-" for character in name.lower()).strip("-")
        text = "-".join(part for part in text.split("-") if part) or "op"
        if name.endswith("<-"):
            text += "-assign"
    return f"{package.lower().replace('.', '-')}-{text}"


def assign_ids(programs):
    """Gives every program a unique id; a clash between two slugs keeps both with a numbered suffix."""
    taken = {}
    for program in programs:
        base = slug(program["package"], program["name"])
        taken[base] = taken.get(base, 0) + 1
        program["id"] = base if taken[base] == 1 else f"{base}-{taken[base]}"


def weigh_programs(programs, codebase):
    """Each program's weight in a code base: its share of the GNU R time the programs stand for
    there (`gnu_time`)."""
    total = sum(program["gnu_time"].get(codebase, 0.0) for program in programs)
    for program in programs:
        program.setdefault("weight", {})
        if total and program["gnu_time"].get(codebase):
            program["weight"][codebase] = program["gnu_time"][codebase] / total


def write_suite(directory, programs, captures, selection):
    samples = directory / "samples"
    shutil.rmtree(samples, ignore_errors=True)
    samples.mkdir(parents=True)
    manifest_programs = []
    for program in programs:
        files = []
        for number, sample in enumerate(program["chosen"], start=1):
            name = f"samples/{program['id']}-{number}.rds"
            shutil.copyfile(sample["path"], directory / name)
            files.append(name)
        manifest_programs.append({
            "id": program["id"], "display": program["display"], "samples": files,
            "iterations": program["iterations"], "rng": program["rng"], "threw": program["threw"], "fresh": program["fresh"],
            "namespaces": program["namespaces"],
            "gnu_seconds_per_call": float(f"{program['gnu_seconds_per_call']:.6g}"),
            "share": {codebase: round(value, 6) for codebase, value in program["share"].items()},
            "weight": {codebase: round(value, 6) for codebase, value in program["weight"].items()},
        })
    coverage = {codebase: round(sum(program["share"].get(codebase, 0) for program in programs), 4) for codebase in captures}
    chosen = {(program["package"], program["name"]) for program in programs}
    captured = {codebase: round(sum(capture["entries"][key]["seconds"] for key in chosen if key in capture["entries"])
                                / (capture["summary"]["total_seconds"] or 1.0), 4)
                for codebase, capture in captures.items()}
    manifest = {
        "id": bench.HOT_SUITE, "name": "Hot package entries",
        "summary": "package calls that dominate the benchmark code bases' time, replayed in a loop",
        "description": ("Each program is one package entry that the three benchmark code bases spend much of their "
                        "time in, with a few of the calls they make to it, captured with their arguments while the "
                        "code bases' tests ran on ry. `tools/bench/hot/harness.R` replays every program's calls in "
                        "batches of about 20 ms of GNU R time in one process per engine, with every package "
                        "the code bases import loaded (`session`). A program's weight in a "
                        "code base is its share of the code base's GNU R time, and the predicted speed-up of a code "
                        "base is the weighted harmonic mean of its programs' speed-ups.\n"),
        "session": sorted({package for capture in captures.values() for package in capture["session"]}, key=str.lower),
        "selection": dict(selection, coverage_of_run=coverage, captured_of_run=captured),
        "programs": manifest_programs,
    }
    (directory / "suite.toml").write_text(render_toml(manifest))
    print(f"wrote {directory / 'suite.toml'} with {len(programs)} programs and {sum(map(len, (p['chosen'] for p in programs)))} samples")


def print_selection(programs, captures):
    chosen = {(program["package"], program["name"]) for program in programs}
    for codebase, capture in captures.items():
        summary, entries = capture["summary"], capture["entries"]
        total = summary["total_seconds"] or 1.0
        covered = sum(map(package_seconds, entries.values()))
        held = sum(package_seconds(entries[key]) for key in chosen if key in entries)
        captured = sum(entries[key]["seconds"] for key in chosen if key in entries)
        print(f"\n{codebase}: the programs hold {held / total:.1%} of the run's time on ry "
              f"({captured / total:.1%} in calls the recorder could capture) and "
              f"{held / covered if covered else 0:.1%} of the package time")
        lost = sorted((key for key in entries if key not in chosen and package_seconds(entries[key]) > 0
                       and not capture["samples"].get(key)), key=lambda key: (-package_seconds(entries[key]), key))
        if lost:
            print("  lost, with no valid sample: " + ", ".join(
                f"{package}::{name} ({package_seconds(entries[(package, name)]) / total:.2%})" for package, name in lost[:10]))
        dominated = sorted((key for key, entry in entries.items() if entry["uncapturable_seconds"] > entry["seconds"]),
                           key=lambda key: (-entries[key]["uncapturable_seconds"], key))
        if dominated:
            print("  mostly uncapturable: " + ", ".join(
                f"{package}::{name} ({entries[(package, name)]['uncapturable_seconds'] / total:.2%} uncapturable, "
                f"{reasons_of(entries[(package, name)])})" for package, name in dominated[:10]))


def reasons_of(entry):
    """Why the recorder could not capture an entry's calls, by how many calls each reason took."""
    reasons = [(entry[field], field.replace("_", " "))
               for field in ("dots", "local_closure", "user_binding", "caller_frame", "uninspectable", "oversize")
               if entry[field]]
    return ", ".join(f"{count:.0f} {reason}" for count, reason in sorted(reasons, key=lambda item: -item[0])) or "no reason"


# Tracking.

def run_track(args):
    """Measures the suite and the code bases on every configuration, then prints the comparison.
    The measurements accumulate in `--out`, so a run that stopped continues where it was."""
    oracle = bench.root / "target/release/oracle"
    if not oracle.exists():
        sys.exit("error: the code bases run through the oracle; build it with `cargo build --release -p oracle`")
    configurations = [parse_configuration(text) for text in args.configurations]
    names = [name for name in args.only.split(",") if name] or list(CODEBASES)
    manifest = bench.load_hot_suite(args.suite)
    programs = manifest["programs"]
    base = {key: value for key, value in os.environ.items() if not key.startswith("RY_")}
    base.update(LC_ALL="C.UTF-8", **bench.library_environment(args.rscript, dependency_library()))
    measured = json.loads(args.out.read_text()) if args.out.exists() else {}
    measured.setdefault("hot", {})
    measured.setdefault("codebases", {})

    def save():
        args.out.parent.mkdir(parents=True, exist_ok=True)
        args.out.write_text(json.dumps(measured, indent=1) + "\n")

    engines = [bench.Engine("gnu_r", "GNU R", [args.rscript, "--vanilla"], base)]
    engines += [bench.Engine(name, name, [binary, "run", "--runtime", "ry"], dict(base, **env))
                for name, binary, env in configurations]
    pending = [engine for engine in engines if engine.key not in measured["hot"]]
    if pending:
        runner = bench.HotRunner(bench.HOT_SUITE, args, directory=args.suite)
        try:
            rounds = {engine.key: {program["id"]: [] for program in programs} for engine in pending}
            for round_ in range(args.rounds):
                for engine in pending:
                    for name, result in runner.run_batch(programs, engine).items():
                        if result.get("seconds") is not None:
                            rounds[engine.key][name].append(result["seconds"])
                    print(f"suite round {round_ + 1} of {args.rounds} on {engine.label}", flush=True)
        finally:
            runner.close()
        for engine in pending:
            # A program that failed in any round has no time: its median would mix fewer runs.
            measured["hot"][engine.key] = {name: statistics.median(times) for name, times in rounds[engine.key].items()
                                           if len(times) == args.rounds}
        save()

    work = pathlib.Path(tempfile.mkdtemp(prefix="ry-hot-track-"))
    codebase_base = dict(base, **bench.codebase_environment(args.rscript))
    try:
        for codebase in names:
            directory = (args.codebases / codebase).resolve()
            if not directory.is_dir():
                sys.exit(f"error: no code base {directory}; write the code bases with tooling/generate.py")
            found = measured["codebases"].setdefault(codebase, {})
            runs = [("gnu_r", configurations[0][1], {})] + configurations
            for name, binary, env in runs:
                if name in found:
                    continue
                engine = bench.Engine("gnu_r" if name == "gnu_r" else "ry", name, None, codebase_base)
                result = bench.run_codebase(oracle, os.path.abspath(binary), directory, engine,
                                            dict(codebase_base, **env), work / f"{codebase}-{name}.json")
                result.pop("blocks", None)
                found[name] = result
                print(f"{codebase} on {name}: {result.get('checksum', result.get('reason'))}, "
                      f"{result['seconds']} s of test blocks", flush=True)
                save()
    finally:
        shutil.rmtree(work, ignore_errors=True)
    print_tracking(measured, programs, [name for name, _, _ in configurations], names)


def parse_configuration(text):
    """`NAME=RY[,VARIABLE=VALUE...]` as the name, the binary and its environment. The binary is
    made absolute, because the suite's harness runs in another working directory."""
    name, separator, rest = text.partition("=")
    binary, *pairs = rest.split(",")
    if not separator or not name or name == "gnu_r" or not binary or any("=" not in pair for pair in pairs):
        sys.exit(f"error: a configuration is NAME=RY[,VARIABLE=VALUE...], and `{text}` is not")
    return name, os.path.abspath(binary), dict(pair.split("=", 1) for pair in pairs)


def predicted_ratio(times, baseline, programs, codebase):
    """ry's time over GNU R's for `programs` in the proportions `codebase` runs them: the mean of
    each program's ratio weighted by its share of the code base's GNU R time. Also the summed
    weight of the programs that carry one. Every program must have a time on both engines."""
    weight = cost = 0.0
    for program in programs:
        share = program["weight"].get(codebase, 0.0)
        if share:
            weight += share
            cost += share * times[program["id"]] / baseline[program["id"]]
    return (cost / weight if weight else None), weight


def tracking_rows(measured, programs, configurations, codebase):
    """Per configuration: the predicted and the measured ratio of ry's time to GNU R's for
    `codebase`, and each one's change against the first configuration as a difference of logs.

    The prediction of a configuration and the reference prediction it is compared with are both
    taken over the programs that the configuration, the first configuration and GNU R all ran,
    whose summed weight is `weight`: a configuration that lost a program would otherwise be
    predicted over less of the code base than the reference."""
    runs = measured["codebases"].get(codebase, {})
    gnu_r = (runs.get("gnu_r") or {}).get("seconds")
    times = {name: measured["hot"].get(name, {}) for name in (*configurations, "gnu_r")}
    first = configurations[0]
    rows = []
    for name in configurations:
        common = [program for program in programs
                  if all(times[key].get(program["id"]) for key in (name, first, "gnu_r"))]
        predicted, weight = predicted_ratio(times[name], times["gnu_r"], common, codebase)
        reference, _ = predicted_ratio(times[first], times["gnu_r"], common, codebase)
        seconds = (runs.get(name) or {}).get("seconds")
        rows.append({"name": name, "predicted": predicted, "reference": reference, "weight": weight,
                     "measured": seconds / gnu_r if seconds and gnu_r else None,
                     "checksum": (runs.get(name) or {}).get("checksum", "failed")})
    measured_reference = rows[0]["measured"]
    for row in rows:
        complete = all((row["predicted"], row["reference"], row["measured"], measured_reference))
        row["predicted_change"] = math.log(row["predicted"] / row["reference"]) if complete else None
        row["measured_change"] = math.log(row["measured"] / measured_reference) if complete else None
    return rows


def correlation(pairs):
    """Pearson's r of the pairs, and the slope of the least-squares line through the origin: how
    much of a predicted change shows up in the measurement. None with fewer than three pairs."""
    if len(pairs) < 3:
        return None, None
    mean_x = sum(x for x, _ in pairs) / len(pairs)
    mean_y = sum(y for _, y in pairs) / len(pairs)
    spread_x = sum((x - mean_x) ** 2 for x, _ in pairs)
    spread_y = sum((y - mean_y) ** 2 for _, y in pairs)
    squares = sum(x * x for x, _ in pairs)
    if not spread_x or not spread_y:
        return None, None
    r = sum((x - mean_x) * (y - mean_y) for x, y in pairs) / math.sqrt(spread_x * spread_y)
    return r, sum(x * y for x, y in pairs) / squares


def print_tracking(measured, programs, configurations, codebases):
    pooled = []
    for codebase in codebases:
        rows = tracking_rows(measured, programs, configurations, codebase)
        print(f"\n{codebase} (time over GNU R's; change against {configurations[0]}; weight: the code base's weight "
              f"in the programs that configuration, {configurations[0]} and GNU R all ran, which both predictions use)")
        print("| configuration | predicted | measured | predicted change | measured change | weight | tests |")
        print("| --- | ---: | ---: | ---: | ---: | ---: | --- |")
        for row in rows:
            def ratio(value):
                return "n/a" if value is None else f"{value:.3f}"

            def change(value):
                return "n/a" if value is None else f"{math.expm1(value):+.1%}"
            print(f"| {row['name']} | {ratio(row['predicted'])} | {ratio(row['measured'])} | "
                  f"{change(row['predicted_change'])} | {change(row['measured_change'])} | {row['weight']:.0%} | "
                  f"{row['checksum']} |")
        pairs = [(row["predicted_change"], row["measured_change"]) for row in rows[1:]
                 if row["predicted_change"] is not None]
        pooled += pairs
        print_correlation(codebase, pairs)
    if len(codebases) > 1:
        print()
        print_correlation("all code bases", pooled)


def print_correlation(label, pairs):
    r, slope = correlation(pairs)
    if r is None:
        print(f"{label}: too few configurations for a correlation ({len(pairs)} changes)")
    else:
        print(f"{label}: r = {r:.3f} between the predicted and the measured log changes of {len(pairs)} "
              f"configurations; a predicted change shows up {slope:.2f} times in the measurement")


# Files.

def dependency_library():
    """The code bases' dependency closure, which every step loads packages from. Without it GNU R
    would fall back to its own library, which may hold other versions or lack packages."""
    library = bench.codebase_library()
    if not library.is_dir():
        sys.exit(f"error: no dependency closure at {library}; run `just corpus`, or point RY_CORPUS_DIR at one")
    return library


def capture_directories(captures):
    directories = [captures / name for name in CODEBASES if (captures / name / "samples.tsv").exists()]
    if not directories:
        sys.exit(f"error: {captures} holds no capture; run `hot.py capture` first")
    return directories


def read_summary(directory):
    """The recorder's `summary.tsv`, with each `*_nanoseconds` key read as `*_seconds`."""
    summary = {}
    for line in (directory / "summary.tsv").read_text().splitlines()[1:]:
        if line.strip():
            key, value = line.split("\t")
            summary[seconds_field(key)] = int(value) / 1e9 if key.endswith("nanoseconds") else float(value)
    return summary


NUMBER_FIELDS = {"calls", "samples", "uncapturable_calls", "dots", "local_closure", "user_binding", "caller_frame",
                 "uninspectable", "oversize", "reentered", "gnu_seconds_per_call", "iterations", "bytes"}
NANOSECOND_FIELDS = {"nanoseconds", "uncapturable_nanoseconds"}
FLAG_FIELDS = {"valid", "rng", "threw", "fresh"}


def seconds_field(name):
    """The name a recorder column in integer nanoseconds is read under, as seconds."""
    return name.removesuffix("nanoseconds") + "seconds" if name.endswith("nanoseconds") else name


def read_tsv(path):
    """A recorder or validator table, with numbers as floats, the recorder's times (integer
    nanoseconds, in the columns `nanoseconds` and `uncapturable_nanoseconds`) as seconds under the
    names `seconds` and `uncapturable_seconds`, the flags as booleans and the list of namespaces
    split."""
    rows = []
    with path.open(newline="") as handle:
        for row in csv.DictReader(handle, delimiter="\t", quoting=csv.QUOTE_NONE):
            parsed = {}
            for key, value in row.items():
                if key in NUMBER_FIELDS:
                    parsed[key] = float(value) if value not in ("", "NA") else math.nan
                elif key in NANOSECOND_FIELDS:
                    parsed[seconds_field(key)] = int(value) / 1e9
                elif key in FLAG_FIELDS:
                    parsed[key] = value in ("TRUE", "true", "1")
                elif key == "namespaces":
                    parsed[key] = [name for name in value.split(",") if name]
                else:
                    parsed[key] = value
            rows.append(parsed)
    return rows


def render_toml(manifest):
    """The suite manifest as TOML: top-level values, then the `selection` table, then one
    `[[programs]]` table per program, which is all the shape the suite has."""
    lines = [f"{key} = {toml_value(value)}" for key, value in manifest.items() if key not in ("selection", "programs")]
    lines += ["", "[selection]"] + [f"{key} = {toml_value(value)}" for key, value in manifest["selection"].items()]
    for program in manifest["programs"]:
        lines += ["", "[[programs]]"] + [f"{key} = {toml_value(value)}" for key, value in program.items()]
    return "\n".join(lines) + "\n"


def toml_value(value):
    if isinstance(value, bool):
        return "true" if value else "false"
    if isinstance(value, (int, float)):
        return repr(value)
    if isinstance(value, str):
        # A JSON string is a TOML basic string, escapes and all.
        return json.dumps(value, ensure_ascii=False)
    if isinstance(value, dict):
        return "{ " + ", ".join(f"{toml_key(key)} = {toml_value(item)}" for key, item in value.items()) + " }" if value else "{}"
    return "[" + ", ".join(toml_value(item) for item in value) + "]"


def toml_key(key):
    return key if key.replace("-", "").replace("_", "").isalnum() and key.isascii() else json.dumps(key)


if __name__ == "__main__":
    main()
