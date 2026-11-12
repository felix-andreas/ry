#!/usr/bin/env python3
"""Measures install-time typed code against first-call compilation.

It writes two packages into a scratch directory, installs them with the given ry binary, and
runs each script below in fresh processes, once with the code image and once with
`RY_CODE_IMAGE=0`:

- `typedfifty`: 50 typed functions, ten shapes over numbers, strings and lists with five
  variants each, which differ in their constants so no two compile to the same code. `first.R`
  calls each once, `skipped.R` is the same script with the calls not run, and `again.R` calls
  each 200 more times.
- `typedslow`: functions for a failed entry, a call whose argument types are not the ones the
  install compiled for. `root` and `poly` declare `double`, so an integer is a legal argument and
  the session compiles a second version for it. `scale` declares an `integer` count, so a double
  count fails the annotation and the call runs in the interpreter. `plain` is untyped, and the
  install compiles it for scalar numbers only.

A script prints its own timed sections. Peak resident memory and CPU time are the child's
`wait4` numbers, which is what `/usr/bin/time -v` reports. The peak counts mapped file pages,
which processes share and whose number depends on what the page cache holds, so each script also
prints its anonymous and file-backed resident memory as it ends (`/proc/self/status`).
`RY_CODE_IMAGE=0` turns off the base packages' images too, so compare each mode's `first.R`
against its own `skipped.R`, not the two modes' totals.

Usage: python3 -I tools/bench/internal/install_time_code.py --ry target/release/ry [--runs 31] [--keep DIR]
"""

import argparse
import collections
import os
import pathlib
import re
import statistics
import subprocess
import sys
import tempfile
import time

TIMED = (
    "started <- Sys.time()\n{body}\n"
    "elapsed <- as.numeric(Sys.time() - started, units = 'secs')\n"
    "cat(sprintf('{label}: %.3f ms\\n', elapsed * 1000))\n"
)
MEMORY = 'cat(grep("RssAnon|RssFile", readLines("/proc/self/status"), value = TRUE), sep = "\\n")\n'
ROUNDS = 20000


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--ry", required=True, help="the ry binary to measure")
    parser.add_argument("--runs", type=int, default=31, help="fresh processes per script and mode")
    parser.add_argument("--keep", help="write the packages here and keep them")
    arguments = parser.parse_args()
    ry = str(pathlib.Path(arguments.ry).resolve())
    with tempfile.TemporaryDirectory() as scratch:
        root = pathlib.Path(arguments.keep or scratch).resolve()
        for name, write in (("fifty", write_fifty), ("slow", write_slow)):
            directory = root / name
            (directory / "pkg" / "R").mkdir(parents=True, exist_ok=True)
            (directory / "library").mkdir(exist_ok=True)
            scripts = write(directory)
            subprocess.run([ry, "install", "pkg", "-l", "library"], cwd=directory, check=True, capture_output=True)
            print(f"## {name}\n")
            print("| script | measure | image | no image |")
            print("| --- | --- | ---: | ---: |")
            for script in scripts:
                with_image = measure(ry, directory, script, "1", arguments.runs)
                without = measure(ry, directory, script, "0", arguments.runs)
                for measure_name in with_image:
                    print(f"| {script} | {measure_name} | {with_image[measure_name]:.3f} | {without[measure_name]:.3f} |")
            print()


def write_fifty(directory):
    functions = []
    calls = []
    for k in range(1, 6):
        functions.append(f"""poly{k} <- function(x: F64) -> F64 {{
  y <- {k}.5
  y <- y * x + {k + 1}.25
  y <- y * x - {k}.75
  y <- y * x + 0.{k}
  y * x - {k * 3}.0
}}""")
        calls.append(f"poly{k}(1.5)")
        functions.append(f"""annuity{k} <- function(periods: I64, rate: F64) -> F64 {{
  balance <- {k}000.0
  paid <- 0.0
  for (period in List.range(1, periods)) {{
    interest <- balance * rate / {k + 11}.0
    payment <- if (balance + interest < {k}50.0) balance + interest else {k}50.0
    balance <- balance + interest - payment
    paid <- paid + payment + poly{k}(rate) * 0.0
  }}
  paid + balance
}}""")
        calls.append(f"annuity{k}(24, 0.05)")
        functions.append(f"""spread{k} <- function(xs: List<F64>) -> F64 {{
  sum <- 0.0
  for (x in xs) {{
    sum <- sum + x
  }}
  mean <- sum / Num.to_f64(List.len(xs))
  squares <- 0.0
  for (x in xs) {{
    squares <- squares + (x - mean) * (x - mean) * {k}.0
  }}
  Num.sqrt(squares / Num.to_f64(List.len(xs)))
}}""")
        calls.append(f"spread{k}(xs)")
        functions.append(f"""scaled{k} <- function(n: I64) -> List<I64> {{
  List.map(List.range(1, n), function(i) i * i * {k} + {k + 2})
}}""")
        calls.append(f"List.len(scaled{k}(40))")
        functions.append(f"""describe{k} <- function(name: Str, count: I64) -> Str {{
  head <- Str.concat("report {k} for the account named ", name)
  body <- Str.concat(head, ": ")
  tail <- if (count > {k}) " entries, more than expected" else " entries"
  Str.concat(Str.concat(body, Num.to_str(count * {k})), tail)
}}""")
        calls.append(f'describe{k}("a", 7)')
        functions.append(f"""divides{k} <- function(n: I64) -> Bool {{
  n %% {k + 1} == 0 || n %% {k + 6} == 0 && n > {k * 10}
}}""")
        calls.append(f"divides{k}(84)")
        functions.append(f"""root{k} <- function(x: F64) -> F64 {{
  guess <- x / {k + 1}.0 + 1.0
  steps <- 0
  while (Num.abs(guess * guess - x) > 1e-{k + 5} && steps < 60) {{
    guess <- (guess + x / guess) / 2.0
    steps <- steps + 1
  }}
  guess
}}""")
        calls.append(f"root{k}(2.0)")
        functions.append(f"""median{k} <- function(xs: List<F64>) -> F64 {{
  clipped <- List.map(xs, function(x) if (x > {k + 4}.5) {k + 4}.5 else x)
  sorted <- List.sort_asc(clipped)
  count <- List.len(sorted)
  middle <- (count + 1) %/% 2
  if (count %% 2 == 0) (sorted[[middle]] + sorted[[middle + 1]]) / 2.0 else sorted[[middle]]
}}""")
        calls.append(f"median{k}(xs)")
        functions.append(f"""series{k} <- function(n: I64) -> I64 {{
  previous <- {k}
  current <- {k + 1}
  for (step in List.range(1, n)) {{
    following <- previous + current
    previous <- current
    current <- following %% 1000003
  }}
  current
}}""")
        calls.append(f"series{k}(30)")
        functions.append(f"""common{k} <- function(a: I64, b: I64) -> I64 {{
  x <- a * {k}
  y <- b
  while (y != 0) {{
    rest <- x %% y
    x <- y
    y <- rest
  }}
  x
}}""")
        calls.append(f"common{k}(84, 36)")
    write_package(directory, "typedfifty", functions)
    header = (
        'library(typedfifty, lib.loc = "library")\n'
        "xs <- List.map(List.range(1, 8), function(i) Num.to_f64(i))\n"
    )
    once = "\n".join(f"value <- {call}" for call in calls)
    (directory / "first.R").write_text(
        header + TIMED.format(body=once, label="first calls of 50 functions") + MEMORY
    )
    (directory / "skipped.R").write_text(
        header + TIMED.format(body="if (FALSE) {\n" + once + "\n}", label="first calls of 50 functions") + MEMORY
    )
    (directory / "again.R").write_text(
        header + once + "\n" + TIMED.format(body="\n".join([once] * 200), label="10,000 more calls") + MEMORY
    )
    return ["skipped.R", "first.R", "again.R"]


def write_slow(directory):
    functions = []
    for k in range(1, 6):
        functions.append(f"""root{k} <- function(x: double) -> double {{
  guess <- x / {k + 1}.0 + 1.0
  steps <- 0
  while (abs(guess * guess - x) > 1e-{k + 5} && steps < 60) {{
    guess <- (guess + x / guess) / 2.0
    steps <- steps + 1
  }}
  guess
}}""")
        functions.append(f"""poly{k} <- function(x: double) -> double {{
  y <- {k}.5
  y <- y * x + {k + 1}.25
  y <- y * x - {k}.75
  y <- y * x + 0.{k}
  y * x - {k * 3}.0
}}""")
        functions.append(f"""scale{k} <- function(x: double, n: integer) -> double {{
  y <- x
  for (i in seq_len(n)) {{
    y <- y * 1.0{k} + {k}.0
  }}
  y
}}""")
        functions.append(f"""plain{k} <- function(x, y) {{
  z <- x * {k}.5 + y
  z <- z * x - {k}.25
  z * y + x
}}""")
    write_package(directory, "typedslow", functions)
    header = 'library(typedslow, lib.loc = "library")\n'
    scripts = []
    for name, shape, call_arguments in (
        ("root-declared", "root", "2.0"),
        ("root-integer", "root", "2L"),
        ("poly-declared", "poly", "1.5"),
        ("poly-integer", "poly", "3L"),
        ("scale-declared", "scale", "2.0, 8L"),
        ("scale-double-count", "scale", "2.0, 8"),
        ("plain-doubles", "plain", "1.5, 2.5"),
        ("plain-logical", "plain", "1.5, TRUE"),
    ):
        calls = [f"{shape}{k}({call_arguments})" for k in range(1, 6)]
        once = "\n".join(f"value <- {call}" for call in calls)
        loop = f"for (round in seq_len({ROUNDS})) {{\n" + "\n".join(f"  value <- {call}" for call in calls) + "\n}"
        (directory / f"{name}.R").write_text(
            header
            + TIMED.format(body=once, label="first calls of 5 functions")
            + TIMED.format(body=loop, label=f"{ROUNDS * 5} more calls")
            + MEMORY
        )
        scripts.append(f"{name}.R")
    return scripts


def write_package(directory, name, functions):
    names = [function.split(" <- ")[0] for function in functions]
    (directory / "pkg" / "DESCRIPTION").write_text(
        f"Package: {name}\nVersion: 1.0.0\nTitle: Install-time code measurement\n"
        "Description: Typed functions for measuring first calls.\nLicense: MIT\n"
    )
    (directory / "pkg" / "NAMESPACE").write_text(f"export({', '.join(names)})\n")
    (directory / "pkg" / "R" / "typed.R").write_text("\n".join(functions) + "\n")


def measure(ry, directory, script, image, runs):
    """The median of each measure over `runs` fresh processes."""
    samples = collections.defaultdict(list)
    for _ in range(runs):
        started = time.perf_counter()
        process = subprocess.Popen(
            [ry, "run", "--runtime", "ry", script],
            cwd=directory,
            env=dict(os.environ, RY_CODE_IMAGE=image),
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            text=True,
        )
        output = process.stdout.read()
        # `wait4` gives this child's own peak and CPU time; `getrusage` for children would give
        # the largest peak of every child so far.
        _, status, usage = os.wait4(process.pid, 0)
        wall = time.perf_counter() - started
        if status != 0:
            sys.exit(f"{script} failed:\n{output}")
        for name, value in re.findall(r"^(.*): ([0-9.]+) ms$", output, re.M):
            samples[f"{name} (ms)"].append(float(value))
        for name, label in (("RssAnon", "anonymous"), ("RssFile", "file-backed")):
            kilobytes = re.search(rf"^{name}:\s+(\d+) kB$", output, re.M)
            samples[f"{label} resident memory at exit (MiB)"].append(int(kilobytes.group(1)) / 1024)
        samples["peak resident memory (MiB)"].append(usage.ru_maxrss / 1024)
        samples["process CPU (ms)"].append((usage.ru_utime + usage.ru_stime) * 1000)
        samples["process wall (ms)"].append(wall * 1000)
    return {name: statistics.median(values) for name, values in samples.items()}


if __name__ == "__main__":
    main()
