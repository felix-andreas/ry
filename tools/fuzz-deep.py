#!/usr/bin/env python3
"""Deep fuzz campaign: every seeded harness in the workspace at a deep budget, within a time limit.

A seeded harness is a `#[test]`, or the `main` of a `harness = false` test binary, that loops
through `syntax::testing::for_each_seed` or `fuzz_seeds`, directly or through a function of the
same file. They are found by reading the sources, so a new harness joins the campaign without an
entry here. Each round runs every harness once on its next window of seeds, through
`FUZZ_SEED=<start> FUZZ_ITERS=<count>`, so successive rounds and successive campaigns with
different `--base` values cover new seeds. A window is `--depth` times the harness's default
seed count, and four times that for the harnesses the campaign weights most: the language-server
simulation, the C API sequences and the collector. The oracle's GC torture joins the rotation
too, choosing its cases by seed.

Before the first round, the libr test suites that build and run the vendored packages' native
code run once under `RY_GC_STRESS=startup` with `RY_GC_VERIFY=1`, which collects on every
allocation and checks the write barrier before each minor collection.

A failure does not stop the campaign. Each one is logged with the command that replays it, and
the script exits 1 if anything failed. The wall clock bounds the campaign only; the harnesses
themselves draw every random choice from their seed.

Usage:
    python3 -I tools/fuzz-deep.py [--minutes 120] [--base 1000000] [--depth 10]
                                  [--log target/fuzz-deep.log] [--only NAME]

`just fuzz-deep` runs it; `just fuzz-deep 480 $(date +%s)` is the shape a scheduled run uses.
"""

import argparse
import json
import os
import re
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
# The oracle's torture chooses this many cases per window; each case is two ry processes.
TORTURE_CASES = 24
RUN_LIMIT = 3600
TEST = re.compile(r"#\[test\]\s*((?:#\[[^\]]*\]\s*)*)fn (\w+)\(\)\s*\{")
# A `harness = false` test binary's own `main`, which runs its seeds when started bare.
MAIN = re.compile(r"\bfn (main)\(\)[^{]*\{")
FUNCTION = re.compile(r"\bfn (\w+)\s*(?:<[^>]*>)?\(([^)]*)\)[^{;]*\{")
SEEDED = re.compile(r"\b(?:for_each_seed|fuzz_seeds)\(")
# The seed count a harness passes when it is not a literal, such as a scaled `fuzz_iterations`.
FALLBACK_SEEDS = 100


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

    record(f"== fuzz-deep: {len(harnesses)} harnesses, base {options.base}, depth {options.depth}")
    if not options.only:
        for suite in GC_STRESS_SUITES:
            if time.monotonic() >= deadline:
                break
            executable = executables.get(("libr", suite))
            if executable is None:
                continue
            environment = {"RY_GC_STRESS": "startup", "RY_GC_VERIFY": "1"}
            ok, output = run(executable, [], environment, "crates/libr", deadline)
            if ok:
                record(f"ok gc-stress {suite}")
            else:
                failures += 1
                record(f"FAIL gc-stress {suite}\n  replay: RY_GC_STRESS=startup RY_GC_VERIFY=1 "
                       f"cargo test -p libr --test {suite}\n{indent(output)}")

    rounds = 0
    while time.monotonic() < deadline:
        for harness in harnesses:
            if time.monotonic() >= deadline:
                break
            executable = executables.get((harness["package"], harness["target"]))
            if executable is None:
                continue
            count = harness["default"] * options.depth * (4 if harness["test"] in HEAVY else 1)
            start = options.base + rounds * count
            environment = {"FUZZ_SEED": str(start), "FUZZ_ITERS": str(count)}
            if harness["test"] == "simulated_sessions_match_a_fresh_start":
                environment["SIMULATION_ACTIONS"] = "150"
            if harness["test"] == "main":
                arguments = []
            else:
                arguments = [harness["test"], "--exact"] + (["--ignored"] if harness["ignored"] else [])
            ok, output = run(executable, arguments, environment, harness["directory"], deadline)
            label = f"{harness['package']} {harness['target']} {harness['test']} seeds {start}..{start + count}"
            if ok:
                record(f"ok {label}")
            else:
                failures += 1
                replays = sorted(set(re.findall(r"FUZZ_SEED=\d+ [^\n]*", output)))
                record(f"FAIL {label}\n  replay: {' | '.join(replays) or replay(harness, environment)}\n{indent(output)}")
        if time.monotonic() < deadline and not options.only:
            executable = executables.get(("oracle", "test_cases"))
            if executable is not None:
                start = options.base + rounds
                environment = {"FUZZ_SEED": str(start), "FUZZ_ITERS": str(TORTURE_CASES)}
                ok, output = run(executable, ["cases_survive_gc_torture", "--exact", "--nocapture"],
                                 environment, "crates/oracle", deadline)
                if ok:
                    record(f"ok oracle gc torture seed {start}")
                else:
                    failures += 1
                    record(f"FAIL oracle gc torture seed {start}\n  replay: FUZZ_SEED={start} "
                           f"FUZZ_ITERS={TORTURE_CASES} cargo test -p oracle --test test_cases "
                           f"cases_survive_gc_torture -- --nocapture\n{indent(output)}")
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
    """Every seeded `#[test]` with its package, test target, crate directory and default seeds."""
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
                if "for_each_seed" not in source and "fuzz_seeds" not in source:
                    continue
                # A test file directly under `tests/` is its own target; everything else is in the
                # library's test target.
                target = name[:-3] if relative == "tests" else None
                functions = parse_functions(source)
                tests = [(match, match.group(2), "#[ignore" in match.group(1)) for match in TEST.finditer(source)]
                if relative == "tests":
                    tests += [(match, "main", False) for match in MAIN.finditer(source)]
                for match, test, ignored in tests:
                    default = budget(function_body(source, match.end()), functions, source, set())
                    if default is None:
                        continue
                    harnesses.append({
                        "package": package.group(1),
                        "target": target,
                        "test": test,
                        "ignored": ignored,
                        "default": default,
                        "directory": os.path.join("crates", crate),
                    })
    return harnesses


def parse_functions(source):
    """Each function of a file by name: its parameter names and its body."""
    functions = {}
    for match in FUNCTION.finditer(source):
        parameters = [part.split(":")[0].strip() for part in match.group(2).split(",") if ":" in part]
        functions.setdefault(match.group(1), (parameters, function_body(source, match.end())))
    return functions


def budget(body, functions, source, visiting, bindings=None):
    """The default seed count of the first seed loop `body` reaches, directly or through a
    helper of the same file, or `None` when it reaches none."""
    candidates = [(match.start(), match.end(), None) for match in SEEDED.finditer(body)]
    for name, (parameters, _) in functions.items():
        if name in visiting:
            continue
        for match in re.finditer(rf"(?<![\w.]){re.escape(name)}\(", body):
            candidates.append((match.start(), match.end(), name))
    for _, end, helper in sorted(candidates):
        arguments = call_arguments(body, end)
        if helper is None:
            return evaluate(arguments[0] if arguments else "", source, bindings or {})
        parameters, helper_body = functions[helper]
        local = dict(zip(parameters, (evaluate(argument, source, bindings or {}) for argument in arguments)))
        found = budget(helper_body, functions, source, visiting | {helper}, local)
        if found is not None:
            return found
    return None


def call_arguments(text, start):
    """The top-level arguments of the call whose `(` ends at `start`."""
    depth, index, arguments, current = 1, start, [], ""
    while index < len(text) and depth:
        character = text[index]
        if character in "([{":
            depth += 1
        elif character in ")]}":
            depth -= 1
        if depth == 1 and character == ",":
            arguments.append(current.strip())
            current = ""
        elif depth:
            current += character
        index += 1
    if current.strip():
        arguments.append(current.strip())
    return arguments


def evaluate(expression, source, bindings):
    """A seed count written as integer arithmetic over literals, the file's constants and the
    helper's parameters; anything else, such as a scaled `fuzz_iterations`, falls back."""
    def substitute(match):
        word = match.group(0)
        if word in bindings and bindings[word] is not None:
            return str(bindings[word])
        constant = re.search(rf"const {re.escape(word)}: \w+ = ([0-9_]+);", source)
        return constant.group(1).replace("_", "") if constant else word
    text = re.sub(r"[A-Za-z_]\w*", substitute, expression.replace("_", "") if re.fullmatch(r"[0-9_]+", expression) else expression)
    text = re.sub(r"(?<=\d)_(?=\d)", "", text)
    if not re.fullmatch(r"[0-9\s*/+()-]+", text):
        return FALLBACK_SEEDS
    try:
        return max(1, int(eval(text.replace("/", "//"), {"__builtins__": {}}, {})))
    except (SyntaxError, ZeroDivisionError):
        return FALLBACK_SEEDS


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
    # Incremental caches for every test binary of the workspace outgrow a tight disk, and a
    # campaign builds once, so it does without them.
    environment = {**os.environ, "CARGO_INCREMENTAL": "0"}
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


def run(executable, arguments, environment, directory, deadline):
    limit = max(1, min(RUN_LIMIT, deadline - time.monotonic() + 60))
    try:
        finished = subprocess.run(
            [executable] + arguments,
            cwd=os.path.join(ROOT, directory),
            env={**os.environ, **environment},
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            text=True,
            errors="replace",
            timeout=limit,
        )
    except subprocess.TimeoutExpired as expired:
        output = expired.stdout or ""
        if isinstance(output, bytes):
            output = output.decode(errors="replace")
        return False, f"timed out after {limit:.0f}s\n{output}"
    return finished.returncode == 0, finished.stdout


def replay(harness, environment):
    target = "--lib" if harness["target"] is None else f"--test {harness['target']}"
    settings = " ".join(f"{key}={value}" for key, value in environment.items())
    return f"{settings} cargo test -p {harness['package']} {target} {harness['test']} -- --exact"


def indent(output):
    lines = [line for line in output.splitlines() if line.strip()]
    return "\n".join("    " + line for line in lines[-40:])


if __name__ == "__main__":
    main()
