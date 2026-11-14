#!/usr/bin/env python3
"""Compares where GNU R and ry spend the time of one code base's test suite, function by function.

`run` executes the benchmark driver (`crates/oracle/src/bench.R`) on a prepared package copy, the
directory `oracle bench --keep` leaves behind, once on GNU R under `Rprof()` and once on ry with
`RY_PROFILE_STACKS`. `report` reads both stack files and prints, for the samples inside test
blocks (`run_block`), the time per closure (self and inclusive), per builtin and per package on
each engine, with GNU R's time over ry's.

Each engine's samples are scaled to that engine's own sum of block times from the same run, so
the two profilers' different intervals and overheads cancel. "Self" is the innermost closure
together with the builtins it calls, on both engines, because GNU R's `.Internal` builtins run
without a context of their own. A builtin row compares only builtins GNU R gives a context
(true builtins called by name; the bytecode compiler's inlined operators such as `[` and `+`
are part of their caller there).

The "excess" column ranks what keeps the suite from a target speed-up (`--target`, default 3):
ry's time minus GNU R's time over the target. The excesses of all self rows sum to the whole gap.

    python3 tools/bench/compare_profiles.py run /tmp/ry-bench-123-finbench/finbench --out .profile/finbench
    python3 tools/bench/compare_profiles.py report .profile/finbench
"""

import argparse
import collections
import json
import os
import pathlib
import subprocess
import sys

ROOT = pathlib.Path(__file__).resolve().parents[2]
DRIVER = ROOT / "crates" / "oracle" / "src" / "bench.R"
BLOCK = "run_block"


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    commands = parser.add_subparsers(dest="command", required=True)
    run = commands.add_parser("run", help="profile one prepared package on both engines")
    run.add_argument("package", type=pathlib.Path)
    run.add_argument("--out", type=pathlib.Path, required=True)
    run.add_argument("--ry", type=pathlib.Path, default=ROOT / "target" / "release" / "ry")
    run.add_argument("--filter", default="", help="a regular expression test file names must match")
    run.add_argument("--engines", default="gnu-r,ry")
    report = commands.add_parser("report", help="compare the two profiles in a directory")
    report.add_argument("directory", type=pathlib.Path)
    report.add_argument("--top", type=int, default=40)
    report.add_argument("--target", type=float, default=3.0)
    report.add_argument("--times", type=pathlib.Path,
                        help="a directory holding the jsonl files of an unprofiled run, to scale times by instead")
    report.add_argument("--callers", metavar="NAME", help="list who calls the closure or builtin NAME instead")
    arguments = parser.parse_args()
    if arguments.command == "run":
        run_profiles(arguments)
    else:
        print_report(arguments)


def run_profiles(arguments):
    package = arguments.package.resolve()
    out = arguments.out.resolve()
    out.mkdir(parents=True, exist_ok=True)
    name = package.name
    for engine in arguments.engines.split(","):
        environment = dict(os.environ)
        environment.update(
            RY_BENCH_PACKAGE=name,
            RY_BENCH_TESTS=str(package / "tests" / "testthat"),
            RY_BENCH_REPORT=str(out / f"{engine}.jsonl"),
            RY_BENCH_FILTER=arguments.filter,
            NOT_CRAN="true",
            TESTTHAT_CPUS="1",
            LANG="C.UTF-8",
            LC_ALL="C.UTF-8",
            TZ="UTC",
        )
        if engine == "gnu-r":
            environment["RY_BENCH_RPROF"] = str(out / "gnu-r.rprof")
        else:
            environment["RY_PROFILE_SUMMARY"] = str(out / "ry-summary.md")
            environment["RY_PROFILE_STACKS"] = str(out / "ry.stacks")
        command = [str(arguments.ry.resolve()), "load", str(package), "--runtime", engine, "--file", str(DRIVER)]
        print(f"{engine}: {' '.join(command)}", file=sys.stderr)
        with open(out / f"{engine}.stderr", "w") as stderr:
            subprocess.run(command, cwd=package, env=environment, stdin=subprocess.DEVNULL,
                           stdout=subprocess.DEVNULL, stderr=stderr, check=False)


def block_seconds(path):
    total = 0.0
    for line in path.read_text(encoding="utf-8", errors="replace").splitlines():
        try:
            row = json.loads(line)
        except json.JSONDecodeError:
            continue
        if row["test"] not in ("<total>", "<file>", "<setup>"):
            total += row["seconds"]
    return total


class Profile:
    """Samples inside test blocks, aggregated the same way for both engines."""

    def __init__(self):
        self.total = 0
        self.own = collections.Counter()  # innermost closure name, with the builtins it calls
        self.inclusive = collections.Counter()
        self.builtins = collections.Counter()
        self.packages = collections.Counter()  # package of the innermost closure
        self.activities = collections.Counter()
        self.callers = collections.defaultdict(collections.Counter)  # name -> the three frames above it
        self.mix = collections.defaultdict(collections.Counter)  # innermost closure -> activities

    def add(self, frames, leaf_builtin, activity, count):
        """`frames` holds closure names outermost first, as `package::name` or `name`."""
        if BLOCK not in (frame.rpartition("::")[2] for frame in frames):
            return
        inside = frames[[frame.rpartition("::")[2] for frame in frames].index(BLOCK) + 1:]
        self.total += count
        self.activities[activity] += count
        if leaf_builtin:
            self.builtins[leaf_builtin] += count
        innermost = inside[-1] if inside else "(the block itself)"
        self.own[innermost.rpartition("::")[2]] += count
        self.mix[innermost.rpartition("::")[2]][activity] += count
        self.packages[innermost.rpartition("::")[0] or "?"] += count
        names = [frame.rpartition("::")[2] for frame in inside] + ([leaf_builtin] if leaf_builtin else [])
        for name in set(names):
            self.inclusive[name] += count
            position = names.index(name)
            self.callers[name][tuple(names[max(0, position - 3):position])] += count


def read_ry(path):
    profile = Profile()
    for line in path.read_text(encoding="utf-8", errors="replace").splitlines():
        count, activity, stack = line.split("\t", 2)
        frames = [frame for frame in stack.split(";") if frame]
        builtin = activity[len("builtin:"):] if activity.startswith("builtin:") else None
        category = "builtin" if builtin else activity.split(":")[0]
        profile.add(frames, builtin, category, int(count))
    return profile


def primitive_names():
    script = 'cat(Filter(function(n) is.primitive(get(n, baseenv())), ls(baseenv(), all.names = TRUE)), sep = "\\n")'
    output = subprocess.run(["Rscript", "-e", script], capture_output=True, text=True, check=True).stdout
    return set(output.split())


def read_rprof(path, packages_of):
    primitives = primitive_names()
    profile = Profile()
    stacks = collections.Counter()
    with open(path, encoding="utf-8", errors="replace") as lines:
        for line in lines:
            if not line.startswith('"'):
                continue
            stacks[line.strip()] += 1
    for line, count in stacks.items():
        names = [name.strip('"') for name in line.split('" "')]
        names = ["(anonymous)" if name == "<Anonymous>" else name for name in reversed(names)]
        leaf = names[-1] if names and names[-1] in primitives else None
        closures = [name for name in names if name not in primitives]
        # Rprof names no package; the package ry attributes the name to most often stands in.
        frames = [f"{packages_of.get(name, '?')}::{name}" for name in closures]
        profile.add(frames, leaf, "builtin" if leaf else "closure", count)
    return profile


def print_report(arguments):
    directory = arguments.directory
    ry = read_ry(directory / "ry.stacks")
    packages_of = {}
    for line in (directory / "ry.stacks").read_text(encoding="utf-8", errors="replace").splitlines():
        for frame in line.split("\t", 2)[2].split(";"):
            package, _, name = frame.rpartition("::")
            packages_of.setdefault(name, package)
    gnu = read_rprof(directory / "gnu-r.rprof", packages_of)
    times = arguments.times or directory
    ry_seconds = block_seconds(times / "ry.jsonl")
    gnu_seconds = block_seconds(times / "gnu-r.jsonl")
    target = arguments.target

    def scale(profile, seconds):
        return lambda count: seconds * count / profile.total if profile.total else 0.0

    ry_time = scale(ry, ry_seconds)
    gnu_time = scale(gnu, gnu_seconds)
    print(f"# Profile comparison: {directory.name}\n")
    print(f"Block time: GNU R {gnu_seconds:.1f} s, ry {ry_seconds:.1f} s, GNU R over ry {gnu_seconds / ry_seconds:.2f}x.")
    print(f"Samples in blocks: GNU R {gnu.total} (Rprof, which samples no faster than every 10 ms), "
          f"ry {ry.total} (1 ms). Excess is ry's time minus GNU R's over {target:g}.\n")

    def table(title, ry_counts, gnu_counts, keys=None, label="name", packages=True):
        keys = keys or set(ry_counts) | set(gnu_counts)
        rows = []
        for key in keys:
            ours, theirs = ry_time(ry_counts.get(key, 0)), gnu_time(gnu_counts.get(key, 0))
            rows.append((ours - theirs / target, key, ours, theirs))
        rows.sort(reverse=True)
        print(f"## {title}\n")
        print(f"| {label} | package | ry s | GNU R s | GNU R over ry | ry share | excess s |")
        print("| --- | --- | ---: | ---: | ---: | ---: | ---: |")
        for excess, key, ours, theirs in rows[: arguments.top]:
            ratio = f"{theirs / ours:.2f}x" if ours > 0 else "-"
            print(f"| `{key}` | {packages_of.get(key, '') if packages else ''} | {ours:.2f} | {theirs:.2f} | {ratio} | "
                  f"{100 * ours / ry_seconds:.1f}% | {excess:.2f} |")
        print()

    if arguments.callers:
        print(f"## Callers of `{arguments.callers}`\n")
        print("| caller chain (innermost last) | ry s | GNU R s |\n| --- | ---: | ---: |")
        rows = [(ry_time(ry.callers[arguments.callers].get(chain, 0)),
                 gnu_time(gnu.callers[arguments.callers].get(chain, 0)), chain)
                for chain in set(ry.callers[arguments.callers]) | set(gnu.callers[arguments.callers])]
        for ours, theirs, chain in sorted(rows, reverse=True)[: arguments.top]:
            print(f"| {' > '.join(chain)} | {ours:.2f} | {theirs:.2f} |")
        return
    table("Closures by self time, ranked by excess", ry.own, gnu.own)
    table("Closures by inclusive time, ranked by excess", ry.inclusive, gnu.inclusive)
    table("Builtins GNU R gives a context, ranked by excess", ry.builtins, gnu.builtins,
          keys=set(gnu.builtins), label="builtin", packages=False)
    print("## Packages of the innermost closure\n")
    print("| package | ry s | GNU R s | GNU R over ry | excess s |\n| --- | ---: | ---: | ---: | ---: |")
    rows = []
    for package in set(ry.packages) | set(gnu.packages):
        ours, theirs = ry_time(ry.packages.get(package, 0)), gnu_time(gnu.packages.get(package, 0))
        rows.append((ours - theirs / target, package, ours, theirs))
    for excess, package, ours, theirs in sorted(rows, reverse=True):
        ratio = f"{theirs / ours:.2f}x" if ours > 0 else "-"
        print(f"| {package} | {ours:.2f} | {theirs:.2f} | {ratio} | {excess:.2f} |")
    print_kinds(ry, gnu, ry_time, gnu_time, ry_seconds, gnu_seconds, target)
    print("\n## ry's activities inside blocks\n")
    print("| activity | ry s | share |\n| --- | ---: | ---: |")
    for activity, count in ry.activities.most_common():
        print(f"| {activity} | {ry_time(count):.2f} | {100 * count / ry.total:.1f}% |")


KINDS = ("interpreter-bound", "builtin-bound", "native-bound", "mixed", "not run on ry")
INTERPRETER = ("tier 0 bytecode instructions", "closure call setup (matching, promises, frame)",
               "S3 method lookup", "S4 method selection", "AST evaluator (uncompiled code)",
               "JIT-compiled code", "JIT compilation", "variable and function lookup (cache misses)",
               "frame introspection (sys.call, parent.frame, ...)", "condition handling")


def kind_of(mix):
    """Classifies a closure by where ry spends its self time: half or more in one kind of work."""
    total = sum(mix.values())
    if not total:
        return "not run on ry"
    if mix["native"] * 2 >= total:
        return "native-bound"
    if mix["builtin"] * 2 >= total:
        return "builtin-bound"
    if sum(mix[activity] for activity in INTERPRETER) * 2 >= total:
        return "interpreter-bound"
    return "mixed"


def print_kinds(ry, gnu, ry_time, gnu_time, ry_seconds, gnu_seconds, target):
    """Closures grouped by the work ry does in them, with the suite's ratio if one group alone, or
    every group but native code, ran at the target speed-up."""
    ours, theirs = collections.Counter(), collections.Counter()
    for name in set(ry.own) | set(gnu.own):
        kind = kind_of(ry.mix.get(name, collections.Counter()))
        ours[kind] += ry_time(ry.own.get(name, 0))
        theirs[kind] += gnu_time(gnu.own.get(name, 0))
    print("\n## Closures by the kind of work ry does in them\n")
    print(f"A closure is native-, builtin- or interpreter-bound when half or more of ry's self time in it "
          f"is that kind of work. The last column is GNU R's suite time over ry's if that kind alone ran "
          f"{target:g} times faster than GNU R.\n")
    print(f"| kind | ry s | ry share | GNU R s | GNU R over ry | suite with this kind at {target:g}x |")
    print("| --- | ---: | ---: | ---: | ---: | ---: |")
    for kind in KINDS:
        if not ours[kind] and not theirs[kind]:
            continue
        ratio = f"{theirs[kind] / ours[kind]:.2f}x" if ours[kind] else "-"
        fixed = ry_seconds - ours[kind] + theirs[kind] / target
        print(f"| {kind} | {ours[kind]:.2f} | {100 * ours[kind] / ry_seconds:.1f}% | {theirs[kind]:.2f} | "
              f"{ratio} | {gnu_seconds / fixed:.2f}x |")
    rest = sum(theirs[kind] for kind in KINDS if kind != "native-bound")
    fixed = ours["native-bound"] + rest / target
    print(f"\nWith every kind but native-bound at {target:g}x, GNU R over ry is {gnu_seconds / fixed:.2f}x.")


if __name__ == "__main__":
    main()
