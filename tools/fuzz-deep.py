#!/usr/bin/env python3
"""Deep fuzz campaign: every seeded harness in the workspace at a deep budget, within a time limit.

A seeded harness is a `#[test]`, or the `main` of a `harness = false` test binary, that loops
through `syntax::testing::for_each_seed`, `fuzz_seeds` or `fuzz_iterations`, directly or through a
function of the same file. They are found by reading the sources, so a new harness joins the
campaign without an entry here, and each is resolved against the test binary's own `--list`, so a
test inside a module is run by its full path. Each round runs every harness once with
`FUZZ_SEED=<start> FUZZ_DEPTH=<depth>`, which makes the harness run `depth` times its own default
seed count from `start`. Rounds start `STRIDE` seeds apart and campaigns with different `--base`
values start apart too, so every round covers new seeds. The harnesses the campaign weights most
(the language-server simulation, the C API sequences and the collector) run at four times the
depth. The oracle's GC torture joins the rotation too, choosing its cases by seed.

Before the first round, the libr test suites that build and run the vendored packages' native
code run once under `RY_GC_STRESS=startup` with `RY_GC_VERIFY=1`, which collects on every
allocation and checks the write barrier before each minor collection.

A failure does not stop the campaign. Each one is logged with the command that replays it, and
the script exits 1 if anything failed. A run the campaign's own deadline cuts short is logged as
cut, not as a failure. The wall clock bounds the campaign only; the harnesses themselves draw
every random choice from their seed.

Usage:
    python3 -I tools/fuzz-deep.py [--minutes 120] [--base 1000000] [--depth 10]
                                  [--log target/fuzz-deep.log] [--only NAME]

`just fuzz-deep` runs it; `just fuzz-deep 480 $(date +%s)` is the shape a scheduled run uses.
"""

import argparse
import json
import os
import re
import signal
import subprocess
import sys
import time

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
HEAVY = {
    "simulated_sessions_match_a_fresh_start",
    "fuzz_api_sequences_keep_objects_intact",
    "fuzz_nested_non_local_exits",
    "fuzz_object_graphs_survive_stress_collection",
    "fuzz_object_graphs_survive_collection",
}
GC_STRESS_SUITES = [
    "test_native",
    "test_altrep_api",
    "test_frame_altrep",
    "test_serialize_native",
    "test_connections",
    "test_fuzz_native",
]
# Far more seeds than any round runs at a deep budget, so rounds never overlap.
STRIDE = 10_000_000
# The oracle's torture chooses this many cases per window; each case is two ry processes.
TORTURE_CASES = 24
RUN_LIMIT = 3600
TEST = re.compile(r"#\[test\]\s*((?:#\[[^\]]*\]\s*)*)fn (\w+)\(\)\s*\{")
# A `harness = false` test binary's own `main`, which runs its seeds when started bare.
MAIN = re.compile(r"\bfn (main)\(\)[^{]*\{")
FUNCTION = re.compile(r"\bfn (\w+)\s*(?:<[^>]*>)?\([^)]*\)[^{;]*\{")
SEEDED = re.compile(r"\b(?:for_each_seed|fuzz_seeds|fuzz_iterations)\(")
REPLAY = re.compile(r"replay with\n\s+(.+)")


def main():
    options = parse_arguments()
    harnesses = discover()
    if options.only:
        harnesses = [harness for harness in harnesses if options.only in harness["test"]]
    executables = build(harnesses)
    log = open(options.log, "a", encoding="utf-8")
    deadline = time.monotonic() + options.minutes * 60
    failures = 0

    def record(line):
        print(line, flush=True)
        log.write(line + "\n")
        log.flush()

    def report(outcome, label, replay_command, output):
        nonlocal failures
        if outcome == "ok":
            record(f"ok {label}")
        elif outcome == "cut":
            record(f"cut {label} (the campaign deadline stopped it)")
        else:
            failures += 1
            record(f"FAIL {label}\n  replay: {replay_command}\n{indent(output)}")

    resolved = []
    for harness in harnesses:
        executable = executables.get((harness["package"], harness["target"]))
        if executable is None:
            failures += 1
            record(f"FAIL {harness['package']} {harness['target']} {harness['test']}: no test binary was built")
            continue
        if harness["test"] == "main":
            resolved.append({**harness, "executable": executable, "path": None})
            continue
        path = resolve(executable, harness)
        if path is None:
            failures += 1
            record(f"FAIL {harness['package']} {harness['target']} {harness['test']}: the test binary lists no such test")
            continue
        resolved.append({**harness, "executable": executable, "path": path})

    record(f"== fuzz-deep: {len(resolved)} harnesses, base {options.base}, depth {options.depth}")
    if not options.only:
        for suite in GC_STRESS_SUITES:
            if time.monotonic() >= deadline:
                break
            executable = executables.get(("libr", suite))
            if executable is None:
                continue
            environment = {"RY_GC_STRESS": "startup", "RY_GC_VERIFY": "1"}
            outcome, output = run(executable, [], environment, "crates/libr", deadline)
            report(outcome, f"gc-stress {suite}",
                   f"RY_GC_STRESS=startup RY_GC_VERIFY=1 cargo test -p libr --test {suite}", output)

    rounds = 0
    while time.monotonic() < deadline:
        start = options.base + rounds * STRIDE
        for harness in resolved:
            if time.monotonic() >= deadline:
                break
            depth = options.depth * (4 if harness["test"] in HEAVY else 1)
            environment = {"FUZZ_SEED": str(start), "FUZZ_DEPTH": str(depth)}
            if harness["test"] == "simulated_sessions_match_a_fresh_start":
                environment["SIMULATION_ACTIONS"] = "150"
            if harness["path"] is None:
                arguments = []
            else:
                arguments = [harness["path"], "--exact"] + (["--ignored"] if harness["ignored"] else [])
            outcome, output = run(harness["executable"], arguments, environment, harness["directory"], deadline)
            if outcome == "ok" and harness["path"] is not None and "running 1 test" not in output:
                outcome, output = "fail", f"the filter ran no test\n{output}"
            label = f"{harness['package']} {harness['target'] or 'lib'} {harness['path'] or 'main'} from seed {start} at depth {depth}"
            replays = sorted(set(REPLAY.findall(output)))
            report(outcome, label, " | ".join(replays) or replay(harness, environment), output)
        if time.monotonic() < deadline and not options.only:
            executable = executables.get(("oracle", "test_cases"))
            if executable is not None:
                seed = options.base + rounds
                environment = {"FUZZ_SEED": str(seed), "FUZZ_ITERS": str(TORTURE_CASES)}
                outcome, output = run(executable, ["cases_survive_gc_torture", "--exact", "--nocapture"],
                                      environment, "crates/oracle", deadline)
                report(outcome, f"oracle gc torture seed {seed}",
                       f"FUZZ_SEED={seed} FUZZ_ITERS={TORTURE_CASES} cargo test -p oracle --test test_cases "
                       f"cases_survive_gc_torture -- --nocapture", output)
        rounds += 1
    record(f"== fuzz-deep: {rounds} rounds, {failures} failures")
    sys.exit(1 if failures else 0)


def parse_arguments():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--minutes", type=float, default=120)
    parser.add_argument("--base", type=int, default=1_000_000)
    parser.add_argument("--depth", type=int, default=10)
    parser.add_argument("--log", default=os.path.join(ROOT, "target", "fuzz-deep.log"))
    parser.add_argument("--only", help="run only the harnesses whose test name contains this")
    return parser.parse_args()


def discover():
    """Every seeded `#[test]` with its package, test target and crate directory."""
    harnesses = []
    crates = os.path.join(ROOT, "crates")
    for crate in sorted(os.listdir(crates)):
        manifest = os.path.join(crates, crate, "Cargo.toml")
        if not os.path.exists(manifest):
            continue
        package = re.search(r'^name\s*=\s*"([^"]+)"', open(manifest, encoding="utf-8").read(), re.M)
        if package is None:
            continue
        for directory, _, files in os.walk(os.path.join(crates, crate)):
            relative = os.path.relpath(directory, os.path.join(crates, crate))
            if relative.split(os.sep)[0] not in ("src", "tests"):
                continue
            for name in sorted(files):
                if not name.endswith(".rs"):
                    continue
                path = os.path.join(directory, name)
                source = open(path, encoding="utf-8").read()
                if not SEEDED.search(source):
                    continue
                # A test file directly under `tests/` is its own target; everything else is in the
                # library's test target.
                target = name[:-3] if relative == "tests" else None
                functions = {match.group(1): function_body(source, match.end()) for match in FUNCTION.finditer(source)}
                tests = [(match, match.group(2), "#[ignore" in match.group(1)) for match in TEST.finditer(source)]
                if relative == "tests":
                    tests += [(match, "main", False) for match in MAIN.finditer(source)]
                for match, test, ignored in tests:
                    if not seeded(function_body(source, match.end()), functions, {test}):
                        continue
                    harnesses.append({
                        "package": package.group(1),
                        "target": target,
                        "test": test,
                        "ignored": ignored,
                        "directory": os.path.join("crates", crate),
                    })
    return harnesses


def seeded(body, functions, visiting):
    """Whether `body` reaches a seed loop, directly or through a function of the same file."""
    if SEEDED.search(body):
        return True
    for name, helper in functions.items():
        if name in visiting or not re.search(rf"(?<![\w.]){re.escape(name)}\(", body):
            continue
        if seeded(helper, functions, visiting | {name}):
            return True
    return False


def function_body(source, start):
    depth = 1
    index = start
    while depth and index < len(source):
        if source[index] == "{":
            depth += 1
        elif source[index] == "}":
            depth -= 1
        index += 1
    return source[start:index]


def build(harnesses):
    """Builds each package's test targets once, returning executables by (package, target)."""
    targets = {}
    for harness in harnesses:
        targets.setdefault(harness["package"], set()).add(harness["target"])
    targets.setdefault("libr", set()).update(GC_STRESS_SUITES)
    targets.setdefault("oracle", set()).add("test_cases")
    # Incremental caches and full debug info for every test binary of the workspace outgrow a
    # tight disk. A campaign builds once, so it does without the caches, and line tables still
    # give a failure's backtrace its files and lines.
    environment = {**os.environ, "CARGO_INCREMENTAL": "0", "CARGO_PROFILE_DEV_DEBUG": "line-tables-only"}
    # The oracle's torture and several suites run the `ry` binary and ry's runtime.
    subprocess.run(["cargo", "build", "-q", "-p", "ry-lang", "-p", "libr", "-p", "rcmd"],
                   cwd=ROOT, env=environment, check=True)
    executables = {}
    for package, names in sorted(targets.items()):
        command = ["cargo", "test", "-q", "-p", package, "--no-run", "--message-format=json"]
        for name in sorted(names, key=lambda name: name or ""):
            command += ["--lib"] if name is None else ["--test", name]
        built = subprocess.run(command, cwd=ROOT, env=environment, check=True,
                               stdout=subprocess.PIPE, text=True)
        for line in built.stdout.splitlines():
            message = json.loads(line) if line.startswith("{") else {}
            if message.get("reason") != "compiler-artifact" or not message.get("executable"):
                continue
            if not message.get("profile", {}).get("test"):
                continue
            kinds = message["target"]["kind"]
            name = None if "lib" in kinds or "rlib" in kinds else message["target"]["name"]
            executables[(package, name)] = message["executable"]
    return executables


def resolve(executable, harness):
    """The full libtest name of a harness, such as `tests::foo` for a test inside a module."""
    command = [executable, "--list", "--format=terse"] + (["--ignored"] if harness["ignored"] else [])
    listed = subprocess.run(command, cwd=os.path.join(ROOT, harness["directory"]),
                            stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True)
    names = [line[: -len(": test")] for line in listed.stdout.splitlines() if line.endswith(": test")]
    matches = [name for name in names if name == harness["test"] or name.endswith("::" + harness["test"])]
    return matches[0] if len(matches) == 1 else None


def run(executable, arguments, environment, directory, deadline):
    """Runs one harness in its own process group, so a timeout kills every process it started.
    Returns `ok`, `fail`, or `cut` when the campaign deadline, not the run limit, stopped it."""
    remaining = deadline - time.monotonic() + 60
    limit = max(1, min(RUN_LIMIT, remaining))
    process = subprocess.Popen(
        [executable] + arguments,
        cwd=os.path.join(ROOT, directory),
        env={**os.environ, **environment},
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
        errors="replace",
        start_new_session=True,
    )
    try:
        output, _ = process.communicate(timeout=limit)
    except subprocess.TimeoutExpired:
        os.killpg(process.pid, signal.SIGKILL)
        output, _ = process.communicate()
        if limit < RUN_LIMIT:
            return "cut", output
        return "fail", f"timed out after {limit:.0f}s\n{output}"
    return ("ok" if process.returncode == 0 else "fail"), output


def replay(harness, environment):
    target = "--lib" if harness["target"] is None else f"--test {harness['target']}"
    settings = " ".join(f"{key}={value}" for key, value in environment.items())
    if harness["path"] is None:
        return f"{settings} cargo test -p {harness['package']} {target}"
    ignored = " --ignored" if harness["ignored"] else ""
    return f"{settings} cargo test -p {harness['package']} {target} -- {harness['path']} --exact{ignored}"


def indent(output):
    lines = [line for line in output.splitlines() if line.strip()]
    return "\n".join("    " + line for line in lines[-40:])


if __name__ == "__main__":
    main()
