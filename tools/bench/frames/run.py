#!/usr/bin/env python3
"""Measures the benchmark code bases' table modules end to end: each module's untyped program
(data.table) on GNU R and on ry, and its typed program (DataFrame, checked in strict mode) on ry,
on the same inputs. Engines run one after another so they never share the CPU. Prints a Markdown
table of the faster of each step's runs and of each engine's peak resident memory, and refuses
results whose checksums disagree. With --write, the table replaces the one between the
`bench-frames` markers in docs/src/content/docs/development/design/r-runtime.md.

The modules are the `tables`, `market_data` and `trials` templates, generated from one seed in
both variants. data.table and lubridate must be installed in the library named by RY_BENCH_RLIB
(default ~/R/library). Build ry and libR in release mode first (`just bench-frames` does).

Usage: tools/bench/frames/run.py [--rows 1e6] [--runs 2] [--write]
"""
import argparse, math, os, pathlib, random, re, subprocess, sys, tempfile

here = pathlib.Path(__file__).resolve().parent
root = here.parent.parent.parent
page = root / "docs/src/content/docs/development/design/r-runtime.md"

MODULES = [
    ("datascience", "tables"),
    ("finance", "market_data"),
    ("biostatistics", "trials"),
]


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--rows", default="1e6")
    parser.add_argument("--runs", type=int, default=2)
    parser.add_argument("--ry", type=lambda path: str(pathlib.Path(path).resolve()), default=str(root / "target/release/ry"))
    parser.add_argument("--write", action="store_true")
    args = parser.parse_args()
    rows = int(float(args.rows))
    rlib = os.environ.get("RY_BENCH_RLIB", os.path.expanduser("~/R/library"))
    env = dict(os.environ, R_LIBS=rlib, LC_ALL="C.UTF-8")

    with tempfile.TemporaryDirectory(prefix="ry-frames-") as scratch:
        scratch = pathlib.Path(scratch)
        write_reference(scratch / "reference.csv", rows)
        engines = [
            ("data.table on GNU R", ["Rscript", "--vanilla"], "untyped"),
            ("data.table on ry", [args.ry, "run"], "untyped"),
            ("DataFrame on ry, typed", [args.ry, "run"], "typed"),
        ]
        lines = ["| Step | " + " | ".join(name for name, _, _ in engines) + " |"]
        lines.append("|---|" + "---:|" * len(engines))
        totals = [0.0] * len(engines)
        peaks = [0.0] * len(engines)
        mismatches = []
        for domain, template in MODULES:
            files = {variant: generate(scratch, domain, template, variant) for variant in ("untyped", "typed")}
            check_strict(args.ry, files["typed"], env)
            results = []
            for index, (name, command, variant) in enumerate(engines):
                print(f"== {template}: {name}", file=sys.stderr, flush=True)
                best = None
                for _ in range(args.runs):
                    stdout, errors, status, peak = run(
                        command + [str(here / "driver.R"), template, str(files[variant]), str(rows), str(scratch), variant],
                        env,
                    )
                    if status != 0:
                        sys.exit(f"{template} on {name} failed with status {status}:\n{errors}")
                    steps = parse(stdout)
                    best = steps if best is None else {
                        step: (min(seconds, best[step][0]), checksum) for step, (seconds, checksum) in steps.items()
                    }
                    peaks[index] = max(peaks[index], peak)
                results.append(best)
            for step, (_, reference) in results[0].items():
                cells = []
                for index, (name, _, _) in enumerate(engines):
                    seconds, checksum = results[index][step]
                    totals[index] += seconds
                    cells.append(f"{seconds:.3f}")
                    if not math.isclose(checksum, reference, rel_tol=1e-9, abs_tol=1e-6):
                        mismatches.append(f"{template} {step}: {name} gives {checksum!r}, GNU R {reference!r}")
                lines.append(f"| `{template}`: `{step}` | " + " | ".join(cells) + " |")
        lines.append("| total | " + " | ".join(f"{total:.2f}" for total in totals) + " |")
        lines.append("| peak memory (GB) | " + " | ".join(f"{peak:.2f}" for peak in peaks) + " |")
    table = "\n".join(lines)
    print(table)
    if mismatches:
        sys.exit("checksums differ:\n" + "\n".join(mismatches))
    if args.write:
        write(rows, table)


def generate(scratch, domain, template, variant):
    """Generates the module's package from a fixed seed and returns its one module file."""
    out = scratch / variant / template
    command = [sys.executable, "-I", str(root / "tools/bench/generate.py"), "--out", str(out), "--domain", domain,
               "--only", template, "--lines", "1", "--files", "1", "--seed", "7"]
    if variant == "typed":
        command.append("--typed")
    subprocess.run(command, check=True, capture_output=True)
    files = sorted(out.glob(f"*/R/{template.replace('_', '-')}-*.R"))
    if not files:
        sys.exit(f"generate.py wrote no {template} module under {out}")
    return files[0]


def check_strict(ry, module, env):
    """The typed program must check clean in strict mode, or its timing claims nothing."""
    package = module.parent.parent
    output = subprocess.run([ry, "check"], cwd=package, env=env, capture_output=True, text=True)
    if output.returncode != 0:
        sys.exit(f"the typed {module.stem} module does not check clean:\n{output.stdout}{output.stderr}")


def write_reference(path, rows):
    """The CSV `read_reference` reads, written once so every engine reads the same bytes."""
    generator = random.Random(7)
    with path.open("w") as out:
        out.write("symbol,date,price,volume\n")
        for row in range(rows):
            day = 10957 + row % 5000
            out.write(f"SYM{generator.randrange(100)},{day},{generator.uniform(50, 150):.4f},{generator.randrange(1, 10000)}\n")


def run(command, env):
    """Runs one engine and returns its output, its error output, its exit status, and its peak
    resident memory in GB. `wait4` reports the peak of this child alone; Linux reports KiB."""
    with tempfile.TemporaryFile(mode="w+") as errors:
        process = subprocess.Popen(command, env=env, stdout=subprocess.PIPE, stderr=errors, text=True, cwd=root)
        with process.stdout:
            stdout = process.stdout.read()
        _, status, usage = os.wait4(process.pid, 0)
        errors.seek(0)
        return stdout, errors.read(), os.waitstatus_to_exitcode(status), usage.ru_maxrss * 1024 / 1e9


def parse(stdout):
    steps = {}
    for line in stdout.splitlines():
        parts = line.split()
        if len(parts) == 4 and parts[0] == "step":
            steps[parts[1]] = (float(parts[3]), float(parts[2]))
    return steps


def write(rows, table):
    text = page.read_text()
    start, end = "<!-- bench-frames:start -->", "<!-- bench-frames:end -->"
    if start not in text or end not in text:
        sys.exit(f"{page} has no bench-frames markers")
    before, rest = text.split(start, 1)
    _, after = rest.split(end, 1)
    heading = f"{rows:,} rows"
    page.write_text(f"{before}{start}\n{heading}:\n\n{table}\n{end}{after}")


if __name__ == "__main__":
    main()
