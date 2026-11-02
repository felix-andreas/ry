#!/usr/bin/env python3
"""Fuzzes `ry migrate data-table --write` over the generated benchmark code bases and over the
data.table users we have: every migrated file must still parse, a typed code base must still
check clean in strict mode, and every migrated table module must give the same checksums as the
original program.

For each domain the generator writes the untyped and the typed package from one seed, and the
migration rewrites a copy of each in place. Then:

- Every rewritten file must parse under GNU R and under ry (no `syntax-error`).
- Migration must not add an error-level diagnostic anywhere in the package.
- A typed package must check clean in strict mode before and after migration.
- Every instance of the `tables`, `market_data` and `trials` templates in an untyped package runs
  through `tools/bench/frames/driver.R` twice under ry, once from the original file and once from
  the migrated one, and each step's checksum must agree.

Extra code bases (`--corpus DIR`, repeatable) get the parse and diagnostic checks; the oracle's
`datatable__*` cases always do. A directory with a DESCRIPTION or ry.toml is migrated as one
project, and loose `.R` files are copied into a scratch project first.

data.table and lubridate must be installed in the library named by RY_BENCH_RLIB (default
~/R/library). Build ry and libR in release mode first (`just bench-migrate` does).

Usage: tools/bench/migrate/fuzz.py [--lines 300000] [--seed 7] [--rows 2e4] [--instances N]
                                   [--domain NAME|all] [--corpus DIR]... [--keep DIR]
"""
import argparse, json, math, os, pathlib, random, re, shutil, subprocess, sys, tempfile

here = pathlib.Path(__file__).resolve().parent
root = here.parent.parent.parent
driver = root / "tools/bench/frames/driver.R"

DOMAINS = {"finance": "finbench", "datascience": "dsbench", "biostatistics": "biobench"}
# The first function of each table module, named `<function>_<id>`, and the driver module that
# runs the instance.
MODULES = {"make_events": "tables", "fetch_prices": "market_data", "enrol": "trials"}
INSTANCE = re.compile(r"^(" + "|".join(MODULES) + r")_(\d+) <- function", re.M)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--lines", type=int, default=300_000)
    parser.add_argument("--seed", type=int, default=7)
    parser.add_argument("--rows", default="2e4")
    parser.add_argument("--instances", type=int, default=0, help="table module instances to run per package (0: all)")
    parser.add_argument("--domain", default="all", choices=["all", *DOMAINS])
    parser.add_argument("--corpus", action="append", default=[], type=pathlib.Path)
    parser.add_argument("--keep", type=pathlib.Path, help="work in this directory and keep it")
    parser.add_argument("--ry", type=lambda path: str(pathlib.Path(path).resolve()), default=str(root / "target/release/ry"))
    args = parser.parse_args()
    rlib = os.environ.get("RY_BENCH_RLIB", os.path.expanduser("~/R/library"))
    env = dict(os.environ, R_LIBS=rlib, LC_ALL="C.UTF-8")
    domains = list(DOMAINS) if args.domain == "all" else [args.domain]

    if args.keep:
        shutil.rmtree(args.keep, ignore_errors=True)
        args.keep.mkdir(parents=True)
        scratch = args.keep
        failures = run_all(args, env, domains, scratch)
    else:
        with tempfile.TemporaryDirectory(prefix="ry-migrate-") as directory:
            failures = run_all(args, env, domains, pathlib.Path(directory))
    if failures:
        sys.exit(f"{len(failures)} failures:\n" + "\n".join(failures))
    print("migration fuzz: no failures")


def run_all(args, env, domains, scratch):
    failures = []
    rng = random.Random(args.seed)
    for domain in domains:
        for typed in (False, True):
            variant = "typed" if typed else "untyped"
            original = generate(args, scratch / variant / "original", domain, typed)
            migrated = scratch / variant / "migrated" / original.name
            shutil.copytree(original, migrated)
            label = f"{original.name} ({variant})"
            failures += migrate_and_check(args.ry, env, label, original, migrated, strict=typed)
            if not typed:
                failures += compare_checksums(args, env, label, original, migrated, scratch, rng)
    for index, corpus in enumerate([*args.corpus, oracle_cases(scratch)]):
        original = project(corpus, scratch / "corpus" / f"{index}-original")
        migrated = scratch / "corpus" / f"{index}-migrated"
        shutil.copytree(original, migrated)
        failures += migrate_and_check(args.ry, env, str(corpus), original, migrated, strict=False)
    return failures


def generate(args, out, domain, typed):
    command = [sys.executable, "-I", str(root / "tools/bench/generate.py"), "--out", str(out), "--domain", domain,
               "--lines", str(args.lines), "--seed", str(args.seed), "--ry", args.ry]
    if typed:
        command.append("--typed")
    subprocess.run(command, check=True)
    return out / DOMAINS[domain]


def migrate_and_check(ry, env, label, original, migrated, strict):
    """Migrates `migrated` in place and returns the failures of the parse, diagnostic and strict
    checks against the untouched `original`."""
    failures = []
    before = errors(ry, env, original)
    if strict and before:
        failures.append(f"{label}: does not check clean before migration:\n" + "\n".join(sorted(before)[:20]))
    output = subprocess.run([ry, "migrate", "data-table", "--write"], cwd=migrated, env=env, capture_output=True, text=True)
    if output.returncode != 0:
        return failures + [f"{label}: `ry migrate data-table --write` failed:\n{output.stdout}{output.stderr}"]
    changed = [path.relative_to(migrated) for path in sorted(migrated.rglob("*.R"))
               if path.read_bytes() != (original / path.relative_to(migrated)).read_bytes()]
    print(f"{label}: {output.stdout.strip()} ({len(changed)} files changed)", file=sys.stderr, flush=True)
    # A typed package written in ry's inline annotation syntax is not R, so only a file whose
    # original GNU R parses has to parse after migration.
    before_parse = gnu_unparsable(env, original, changed)
    broken = {path: message for path, message in gnu_unparsable(env, migrated, changed).items()
              if path not in before_parse}
    if broken:
        failures.append(f"{label}: migrated code GNU R cannot parse:\n"
                        + "\n".join(f"{path}: {broken[path]}" for path in sorted(broken)[:20]))
    after = errors(ry, env, migrated)
    added = sorted(after - before)
    if added:
        failures.append(f"{label}: migration added {len(added)} errors:\n" + "\n".join(added[:20]))
    return failures


def gnu_unparsable(env, package, files):
    """GNU R's parse error for each of `files` (paths relative to `package`) it rejects."""
    if not files:
        return {}
    paths = [package / path for path in files]
    parse = subprocess.run(
        ["Rscript", "--vanilla", "-e",
         "for (f in commandArgs(TRUE)) tryCatch(parse(f), error = function(e) cat(f, '\\t', gsub('\\n', ' ', conditionMessage(e)), '\\n', sep = ''))",
         *map(str, paths)],
        env=env, capture_output=True, text=True)
    if parse.returncode != 0:
        sys.exit(f"Rscript failed:\n{parse.stderr}")
    found = {}
    for line in parse.stdout.splitlines():
        path, _, message = line.partition("\t")
        found[str(pathlib.Path(path).relative_to(package))] = message
    return found


def errors(ry, env, package):
    """The package's error-level diagnostics (syntax errors always, strict type errors in a typed
    package), keyed by file, code and message so that a rewrite moving a line changes nothing."""
    output = subprocess.run([ry, "check", "--output", "json", "--min-severity", "error"],
                            cwd=package, env=env, capture_output=True, text=True)
    if output.returncode == 2:
        sys.exit(f"`ry check` failed in {package}:\n{output.stderr}")
    found = set()
    for line in output.stdout.splitlines():
        diagnostic = json.loads(line)
        path = pathlib.Path(diagnostic["path"])
        name = path.relative_to(package) if path.is_relative_to(package) else path
        found.add(f"{name}: {diagnostic['code']}: {diagnostic['message']}")
    return found


def compare_checksums(args, env, label, original, migrated, scratch, rng):
    instances = []
    for path in sorted((original / "R").glob("*.R")):
        for function, identifier in INSTANCE.findall(path.read_text()):
            instances.append((MODULES[function], identifier, path.relative_to(original)))
    if args.instances and len(instances) > args.instances:
        instances = sorted(rng.sample(instances, args.instances))
    data = scratch / "data"
    if not (data / "reference.csv").exists():
        data.mkdir(exist_ok=True)
        write_reference(data / "reference.csv", int(float(args.rows)))
    failures = []
    for module, identifier, path in instances:
        results = []
        for package, variant in ((original, "untyped"), (migrated, "migrated")):
            command = [args.ry, "run", str(driver), module, str(package / path), args.rows, str(data), variant, identifier]
            output = subprocess.run(command, cwd=root, env=env, capture_output=True, text=True)
            if output.returncode != 0:
                failures.append(f"{label}: {module} {identifier} ({path}, {variant}) failed:\n{output.stderr[-2000:]}")
                break
            results.append(parse_steps(output.stdout))
        if len(results) < 2:
            continue
        for step, reference in results[0].items():
            checksum = results[1].get(step)
            if checksum is None or not math.isclose(checksum, reference, rel_tol=1e-9, abs_tol=1e-6):
                failures.append(f"{label}: {module} {identifier} ({path}) step {step}: migrated gives {checksum!r}, original {reference!r}")
    print(f"{label}: compared {len(instances)} table module instances", file=sys.stderr, flush=True)
    return failures


def parse_steps(stdout):
    steps = {}
    for line in stdout.splitlines():
        parts = line.split()
        if len(parts) == 4 and parts[0] == "step":
            steps[parts[1]] = float(parts[2])
    return steps


def write_reference(path, rows):
    """The same reference CSV `tools/bench/frames/run.py` writes."""
    generator = random.Random(7)
    with path.open("w") as out:
        out.write("symbol,date,price,volume\n")
        for row in range(rows):
            day = 10957 + row % 5000
            out.write(f"SYM{generator.randrange(100)},{day},{generator.uniform(50, 150):.4f},{generator.randrange(1, 10000)}\n")


def oracle_cases(scratch):
    directory = scratch / "oracle-datatable"
    directory.mkdir(exist_ok=True)
    for case in sorted((root / "crates/oracle/cases").glob("datatable__*.R")):
        shutil.copy(case, directory / case.name)
    return directory


def project(source, destination):
    """A copy of `source` that `ry migrate` treats as one project: a package or ry project as it
    is, and loose files under a fresh ry.toml."""
    if (source / "DESCRIPTION").exists() or (source / "ry.toml").exists():
        shutil.copytree(source, destination)
    else:
        destination.mkdir(parents=True)
        for path in sorted(source.rglob("*.R")):
            target = destination / path.relative_to(source)
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy(path, target)
        (destination / "ry.toml").write_text("")
    return destination


if __name__ == "__main__":
    main()
