#!/usr/bin/env python3
"""The fitness function of typed ry's hill climb (docs/src/content/docs/development/design/language-climb.md).
It scores three things and prints one summary table:

- speed: each benchmark program in tools/bench/why, typed in strict mode on ry, against its Rust
  reference in rust/, and the untyped twin on ry against GNU R. The score is the geometric mean of
  typed ry's time over Rust's, so lower is better and 1 is Rust's speed.
- friendliness: the cases in friendly/, each a small program whose first line says what ry should
  do with it (`# expect: ok`, or `# expect: CODE` for the diagnostic it must report). An `ok` case
  must pass strict mode and, on ry's runtime, print exactly its `#>` lines. Also the suffix burden:
  how many `i64`/`f64` suffixes the typed corpus needs per 100 lines.
- soundness: seeded random programs that strict mode accepts must run without a type error, and
  every value must have, at run time, the type the program declares for it.

Usage: tools/climb/run.py [--ry PATH] [--only speed,friendly,sound] [--seed N] [--programs N]
                          [--filter NAME] [--log NOTE]
`--log` appends the scores and NOTE as one line to log.tsv, the climb's step log.
"""
import argparse, datetime, json, math, os, pathlib, random, re, shutil, subprocess, sys, tempfile

here = pathlib.Path(__file__).resolve().parent
root = here.parent.parent
why = root / "tools/bench/why"
scratch_root = pathlib.Path(tempfile.gettempdir()) / "ry-climb"


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--ry", default=None, help="the ry binary; release if built, else debug")
    parser.add_argument("--only", default="speed,friendly,sound")
    parser.add_argument("--seed", type=int, default=1)
    parser.add_argument("--programs", type=int, default=20, help="random programs for soundness")
    parser.add_argument("--filter", default="", help="speed and friendliness: names containing this")
    parser.add_argument("--log", default=None, help="append the scores and this note to log.tsv")
    args = parser.parse_args()
    ry = args.ry or next(
        (str(path) for path in [root / "target/release/ry", root / "target/debug/ry"] if path.exists()),
        None,
    )
    if ry is None:
        sys.exit("no ry binary: build one with `cargo build --release -p ry-lang -p libr`")
    ry = str(pathlib.Path(ry).resolve())
    environment = dict(os.environ, LC_ALL="C.UTF-8", NO_COLOR="1")
    if "R_HOME" not in environment and shutil.which("R"):
        environment["R_HOME"] = subprocess.run(["R", "RHOME"], capture_output=True, text=True).stdout.strip()
    scratch_root.mkdir(exist_ok=True)
    parts = args.only.split(",")
    scores = {}
    if "friendly" in parts:
        scores.update(friendliness(ry, environment, args.filter))
    if "sound" in parts:
        scores.update(soundness(ry, environment, args.seed, args.programs))
    if "speed" in parts:
        scores.update(speed(ry, environment, args.filter))
    print("\n| score | value |\n| --- | --- |")
    for key, value in scores.items():
        print(f"| {key} | {value} |")
    if args.log is not None:
        commit = subprocess.run(["git", "rev-parse", "--short", "HEAD"], capture_output=True, text=True, cwd=root).stdout.strip()
        fields = [datetime.date.today().isoformat(), commit] + [f"{key}={value}" for key, value in scores.items()] + [args.log]
        with open(here / "log.tsv", "a") as log:
            log.write("\t".join(fields) + "\n")


def project(name, source, strict=True):
    """A throwaway project of one ry.toml and main.R, typed and strict."""
    directory = scratch_root / name
    directory.mkdir(parents=True, exist_ok=True)
    (directory / "ry.toml").write_text("[check]\ntyping = true\n" + ("strict = true\n" if strict else ""))
    (directory / "main.R").write_text(source)
    return directory


def ry_check(ry, environment, directory):
    """The errors `ry check` reports, as (code, line, message)."""
    result = subprocess.run([ry, "check", "--output", "json", "main.R"], cwd=directory, capture_output=True, text=True, env=environment)
    findings = []
    for line in result.stdout.splitlines():
        try:
            finding = json.loads(line)
        except json.JSONDecodeError:
            return [("unreadable-output", 0, result.stdout + result.stderr)]
        # Warnings do not stop strict mode from running the code, so only errors count.
        if finding.get("severity") == "error":
            findings.append((finding.get("code"), finding.get("line", 0), finding.get("message", "")))
    return findings


def ry_run(ry, environment, directory, timeout=120):
    result = subprocess.run([ry, "run", "--runtime", "ry", "main.R"], cwd=directory, capture_output=True, text=True, env=environment, timeout=timeout)
    return result.returncode, result.stdout, result.stderr


def friendliness(ry, environment, name_filter):
    print("## Friendliness")
    cases = sorted(path for path in (here / "friendly").glob("*.R") if name_filter in path.stem)
    passed = 0
    for path in cases:
        text = path.read_text()
        expect = text.splitlines()[0].removeprefix("# expect:").strip()
        directory = project(f"friendly-{path.stem}", text)
        codes = [code for code, _, _ in ry_check(ry, environment, directory)]
        problem = None
        if expect == "ok":
            if codes:
                problem = f"reported {', '.join(codes)}"
            else:
                expected = [line[3:] for line in text.splitlines() if line.startswith("#> ")]
                status, out, err = ry_run(ry, environment, directory)
                actual = [line for line in out.splitlines() if line.strip()]
                if status != 0:
                    problem = f"run failed: {(err.strip().splitlines() or ['?'])[-1]}"
                elif expected and actual != expected:
                    problem = f"printed {actual}, expected {expected}"
        elif codes != [expect]:
            problem = f"reported {codes or 'nothing'}, expected [{expect}]"
        passed += problem is None
        print(f"- {path.stem}: {'pass' if problem is None else problem}")
    lines = suffixes = 0
    for path in (why / "typed").glob("*.R"):
        text = path.read_text()
        lines += len([line for line in text.splitlines() if line.strip() and not line.lstrip().startswith("#")])
        suffixes += len(re.findall(r"\b\d[\d.e]*(?:i64|f64)\b", text))
    return {
        "friendly": f"{passed}/{len(cases)}",
        "suffixes per 100 lines": f"{100 * suffixes / max(lines, 1):.1f}",
    }


# The types a random program declares and the `typeof()` each has at run time.
RUNTIME_TYPE = {"I64": "I64", "F64": "double", "Bool": "logical", "Str": "character", "List<I64>": "I64", "List<F64>": "double"}


class Generator:
    """Random well-typed expressions that lean on unsuffixed literals, the source of most past
    soundness bugs: each must end up as the type its context declares."""

    def __init__(self, rng):
        self.rng = rng
        self.variables = {kind: [] for kind in RUNTIME_TYPE}
        self.undecided = False

    def literal(self, kind):
        rng = self.rng
        if kind == "I64":
            value = rng.randint(0, 9)
            return rng.choice([str(value), f"{value}i64", f"{value}L"])
        value = rng.choice(["0.5", "1.25", "2", "3", "0.1"])
        if "." in value:
            return rng.choice([value, f"{value}f64"])
        # A bare whole literal is an F64 only where its context asks for one; at an unannotated
        # top-level binding nothing does, and it is an I64.
        forms = [f"{value}.0", f"{value}f64"] if self.undecided else [value, f"{value}.0", f"{value}f64"]
        return rng.choice(forms)

    def expression(self, kind, depth):
        rng = self.rng
        choices = ["literal"]
        if self.variables[kind]:
            choices.append("variable")
        if depth > 0:
            choices += {
                "I64": ["arith", "if", "match", "call", "len", "sum"],
                "F64": ["arith", "divide", "if", "match", "call", "convert", "sum"],
                "Bool": ["compare", "and", "not"],
                "Str": ["fstring"],
                "List<I64>": ["list", "map", "range"],
                "List<F64>": ["list", "map"],
            }[kind]
        choice = rng.choice(choices)
        inner = depth - 1
        if choice == "variable":
            return rng.choice(self.variables[kind])
        if choice == "literal":
            if kind == "Bool":
                return rng.choice(["TRUE", "FALSE"])
            if kind == "Str":
                return rng.choice(['"a"', '"bc"'])
            if kind.startswith("List"):
                item = kind[5:-1]
                return "[" + ", ".join(self.literal(item) for _ in range(rng.randint(1, 3))) + "]"
            return self.literal(kind)
        if choice == "arith":
            operator = rng.choice(["+", "-", "*"])
            return f"({self.expression(kind, inner)} {operator} {self.expression(kind, inner)})"
        if choice == "divide":
            return f"({self.expression('F64', inner)} / {rng.choice(['2', '4', '0.5'])})"
        if choice == "if":
            return f"(if ({self.expression('Bool', inner)}) {self.expression(kind, inner)} else {self.expression(kind, inner)})"
        if choice == "match":
            return f"(match (List.first({self.expression('List<' + kind + '>', inner)})) {{ :Ok(x) => x, :Err(_) => {self.literal(kind)} }})"
        if choice == "call":
            return f"same_{kind.lower()}({self.expression(kind, inner)})"
        if choice == "len":
            return f"List.len({self.expression('List<' + rng.choice(['I64', 'F64']) + '>', inner)})"
        if choice == "sum":
            return f"List.sum({self.expression('List<' + kind + '>', inner)})"
        if choice == "convert":
            return f"Num.to_f64({self.expression('I64', inner)})"
        if choice == "compare":
            number = rng.choice(["I64", "F64"])
            operator = rng.choice(["<", ">", "==", "!="])
            return f"({self.expression(number, inner)} {operator} {self.expression(number, inner)})"
        if choice == "and":
            return f"({self.expression('Bool', inner)} && {self.expression('Bool', inner)})"
        if choice == "not":
            return f"!{self.expression('Bool', inner)}"
        if choice == "fstring":
            return f'f"n={{{self.expression("I64", inner)}}}"'
        if choice == "list":
            item = kind[5:-1]
            return "[" + ", ".join(self.expression(item, inner) for _ in range(rng.randint(1, 3))) + "]"
        if choice == "map":
            item = kind[5:-1]
            return f"List.map({self.expression(kind, inner)}, \\(v) v * {self.literal(item)})"
        if choice == "range":
            return f"List.range({self.literal('I64')}, {self.rng.randint(3, 6)})"
        raise ValueError(choice)

    def program(self, statements):
        """Statements that bind and print values, and the run-time type each print must show."""
        lines = [
            "same_i64 <- function(x: I64) -> I64 x",
            "same_f64 <- function(x: F64) -> F64 x",
        ]
        expected = []
        for index in range(statements):
            kind = self.rng.choice(list(RUNTIME_TYPE))
            name = f"v{index}"
            annotated = self.rng.random() < 0.5
            self.undecided = not annotated
            value = self.expression(kind, self.rng.randint(0, 3))
            if annotated:
                lines.append(f"{name}: {kind} <- {value}")
            else:
                # Unannotated: the binding's type comes from the value or from the later use below.
                lines.append(f"{name} <- {value}")
            use = {"I64": f"same_i64({name})", "F64": f"same_f64({name})"}.get(kind, name)
            lines.append(f'cat({index}, typeof({use}), "\\n")')
            self.variables[kind].append(name)
            expected.append(RUNTIME_TYPE[kind])
        return "\n".join(lines) + "\n", expected


def soundness(ry, environment, seed, programs):
    print("## Soundness")
    rng = random.Random(seed)
    violations = rejected = 0
    for number in range(programs):
        source, expected = Generator(rng).program(12)
        directory = project(f"sound-{number}", source)
        findings = ry_check(ry, environment, directory)
        if findings:
            rejected += 1
            code, row, message = findings[0]
            print(f"- program {number} (seed {seed}): rejected, {code} at line {row}: {message[:120]}")
            continue
        status, out, err = ry_run(ry, environment, directory)
        actual = {}
        for line in out.splitlines():
            index, _, kind = line.strip().partition(" ")
            if index.isdigit():
                actual[int(index)] = kind
        wrong = [index for index, kind in enumerate(expected) if actual.get(index) != kind]
        if status != 0 or wrong:
            violations += 1
            detail = f"statement {wrong[0]} is {actual.get(wrong[0], 'missing')}, declared {expected[wrong[0]]}" if wrong else ""
            print(f"- program {number} (seed {seed}): UNSOUND, {detail} {(err.strip().splitlines() or [''])[-1]}")
            print("  replay: " + str(directory / "main.R"))
    print(f"- {programs} programs, {rejected} rejected, {violations} unsound")
    return {"unsound": f"{violations}/{programs - rejected}", "rejected": f"{rejected}/{programs}"}


def speed(ry, environment, name_filter):
    print("## Speed")
    rustc = shutil.which("rustc")
    ratios_rust, ratios_r = [], []
    print("| program | typed ry | Rust | typed ry / Rust | untyped ry | GNU R |\n| --- | --- | --- | --- | --- | --- |")
    for path in sorted((here / "rust").glob("*.rs")):
        name = path.stem
        if name_filter not in name:
            continue
        binary = scratch_root / f"rust-{name}"
        if rustc and (not binary.exists() or binary.stat().st_mtime < path.stat().st_mtime):
            subprocess.run([rustc, "-O", "--edition", "2024", "-o", str(binary), str(path)], check=True)
        rust = timed([str(binary)], environment)
        typed = timed_r([ry, "run", "--runtime", "ry"], "typed", name, environment)
        untyped = timed_r([ry, "run", "--runtime", "ry"], "untyped", name, environment)
        gnu_r = timed_r(["Rscript"], "untyped", name, environment) if shutil.which("Rscript") else None
        if typed and rust:
            ratios_rust.append(typed / rust)
        if untyped and gnu_r:
            ratios_r.append(untyped / gnu_r)
        ratio = f"{typed / rust:.1f}" if typed and rust else "-"
        print(f"| {name} | {seconds(typed)} | {seconds(rust)} | {ratio} | {seconds(untyped)} | {seconds(gnu_r)} |")
    return {
        "typed ry / Rust (geomean)": geomean(ratios_rust),
        "untyped ry / GNU R (geomean)": geomean(ratios_r),
    }


def timed(command, environment):
    """The best warm time a program prints as `name checksum best cold`, or None."""
    result = subprocess.run(command, capture_output=True, text=True, env=environment)
    lines = [line for line in result.stdout.splitlines() if line.strip()]
    if result.returncode != 0 or not lines:
        return None
    return float(lines[-1].split()[2])


def timed_r(command, variant, name, environment):
    source = why / variant / f"{name}.R"
    if not source.exists():
        return None
    directory = scratch_root / f"speed-{variant}"
    directory.mkdir(exist_ok=True)
    if (why / variant / "ry.toml").exists():
        shutil.copy(why / variant / "ry.toml", directory / "ry.toml")
    (directory / "main.R").write_text((why / variant / "harness.R").read_text() + "\n" + source.read_text())
    result = subprocess.run(command + ["main.R"], cwd=directory, capture_output=True, text=True, env=dict(environment, R_DEFAULT_PACKAGES="stats"))
    lines = [line for line in result.stdout.splitlines() if line.strip()]
    if result.returncode != 0 or not lines:
        return None
    return float(lines[-1].split()[2])


def seconds(value):
    return "-" if value is None else f"{value:.4f}"


def geomean(values):
    return f"{math.exp(sum(math.log(max(value, 1e-9)) for value in values) / len(values)):.2f}" if values else "-"


if __name__ == "__main__":
    main()
