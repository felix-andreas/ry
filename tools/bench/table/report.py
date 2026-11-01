#!/usr/bin/env python3
"""Runs the table benchmark under data.table, as R code on ry's DataFrame, and on the ry engine from
Rust, one engine after the other so they never share the CPU, and prints a Markdown table of the
faster of each question's two runs. With --write, the table replaces the one between the
`bench-table:<rows>` markers in docs/src/content/docs/contributing/design/r-runtime.md.

The data is generated on first use. data.table must be installed in the library named by
RY_BENCH_RLIB (default ~/R/library). Build ry, libR and the Rust driver first (`just bench-table`
does).

Usage: tools/bench/table/report.py <data-dir> [--rows 1e7] [--groupby-only] [--write]
"""
import argparse, json, math, os, pathlib, re, subprocess, sys

here = pathlib.Path(__file__).resolve().parent
root = here.parent.parent.parent
page = root / "docs/src/content/docs/contributing/design/r-runtime.md"

LABELS = {
    "groupby q1": "groupby q1: `sum(v1)` by `id1`",
    "groupby q2": "groupby q2: `sum(v1)` by `id1, id2`",
    "groupby q3": "groupby q3: `sum(v1)`, `mean(v3)` by `id3`",
    "groupby q4": "groupby q4: three means by `id4`",
    "groupby q5": "groupby q5: three sums by `id6`",
    "groupby q6": "groupby q6: `median(v3)`, `sd(v3)` by `id4, id5`",
    "groupby q7": "groupby q7: `max(v1) - min(v2)` by `id3`",
    "groupby q8": "groupby q8: the two largest `v3` by `id6`",
    "groupby q9": "groupby q9: `cor(v1, v2)^2` by `id2, id4`",
    "groupby q10": "groupby q10: `sum(v3)`, count by all six ids",
    "update full": "`:=` on a whole column, 100 times",
    "update subset": "`:=` on 1% of rows, 100 times",
    "update add": "`:=` adding a column, 100 times",
    "join q1": "join q1: small, on `id1`",
    "join q2": "join q2: medium, on `id2`",
    "join q3": "join q3: medium, left, on `id2`",
    "join q4": "join q4: medium, on factor `id5`",
    "join q5": "join q5: big, on `id3`",
    "rolljoin keyed": "keyed rolling join",
}


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("dir")
    parser.add_argument("--rows", default="1e7")
    parser.add_argument("--groupby-only", action="store_true")
    parser.add_argument("--write", action="store_true")
    args = parser.parse_args()
    data = pathlib.Path(args.dir).resolve()
    rlib = os.environ.get("RY_BENCH_RLIB", os.path.expanduser("~/R/library"))
    env = dict(os.environ, RY_BENCH_RLIB=rlib)
    only = ["groupby"] if args.groupby_only else []

    prepare(data, args.rows, only, env)
    engines = [
        ("data.table", ["Rscript", str(here / "datatable.R"), str(data), *only]),
        ("ry, from R", [str(root / "target/release/ry"), "run", str(here / "ry.R"), str(data), *only]),
        ("ry, from Rust", [rust_driver(), str(data), *(["--groupby-only"] if only else [])]),
    ]
    results = {}
    for name, command in engines:
        print(f"== {name}", file=sys.stderr, flush=True)
        output = subprocess.run(command, env=env, capture_output=True, text=True, cwd=root)
        sys.stderr.write(output.stdout)
        if output.returncode != 0:
            sys.exit(f"{name} failed:\n{output.stderr}")
        results[name] = parse(output.stdout)

    questions = list(results["data.table"])
    lines = ["| Question | " + " | ".join(results) + " |", "|---|" + "---|" * len(results)]
    totals = {name: 0.0 for name in results}
    mismatches = []
    for question in questions:
        cells = []
        reference = results["data.table"][question][1]
        for name, answers in results.items():
            seconds, checksum = answers[question]
            totals[name] += seconds
            cells.append(f"{seconds:.3f}")
            agree = math.isclose(checksum, reference, rel_tol=1e-6, abs_tol=1e-6)
            if not (agree or math.isnan(checksum) and math.isnan(reference)):
                mismatches.append(f"{question}: {name} gives {checksum:.6e}, data.table {reference:.6e}")
        lines.append(f"| {LABELS.get(question, question)} | " + " | ".join(cells) + " |")
    lines.append("| total | " + " | ".join(f"{total:.2f}" for total in totals.values()) + " |")
    table = "\n".join(lines)
    print(table)
    if mismatches:
        sys.exit("checksums differ:\n" + "\n".join(mismatches))
    if args.write:
        write(args.rows, table)


def prepare(data, rows, only, env):
    """Generates whatever data the run needs, and refuses a directory generated at another size,
    whose numbers would land under the wrong heading."""
    needed = ["groupby.csv"]
    if not only:
        needed += [f"join_{name}.csv" for name in ("x", "small", "medium", "big")]
        needed += ["rolljoin_prices.csv", "rolljoin_trades.csv"]
    stamp = data / "rows"
    if (data / "groupby.csv").exists() and not stamp.exists():
        with open(data / "groupby.csv", "rb") as file:
            stamp.write_text(f"{sum(1 for _ in file) - 1}\n")
    if stamp.exists() and int(stamp.read_text()) != int(float(rows)):
        sys.exit(f"{data} holds {stamp.read_text().strip()} rows, not {rows}")
    if not all((data / name).exists() for name in needed):
        subprocess.run(["Rscript", str(here / "datagen.R"), str(data), rows, *only], env=env, check=True)
        stamp.write_text(f"{int(float(rows))}\n")


def rust_driver():
    build = subprocess.run(
        ["cargo", "build", "--release", "-p", "table", "--bench", "db_benchmark", "--message-format=json"],
        cwd=root, capture_output=True, text=True, check=True,
    )
    for line in build.stdout.splitlines():
        message = json.loads(line)
        if message.get("reason") == "compiler-artifact" and message.get("executable") and message["target"]["name"] == "db_benchmark":
            return message["executable"]
    sys.exit("could not find the db_benchmark executable")


def parse(stdout):
    """Maps each question to (the faster of its runs' seconds, the first run's checksum). The `:=`
    loops modify the table, so only the first runs' answers are comparable across engines."""
    answers = {}
    for line in stdout.splitlines():
        if not line.strip() or line.startswith("#"):
            continue
        question, run, seconds, _, _, checksum = line.split("\t")
        best = answers.get(question)
        if best is None:
            answers[question] = (float(seconds), float(checksum))
        elif float(seconds) < best[0]:
            answers[question] = (float(seconds), best[1])
    return answers


def write(rows, table):
    text = page.read_text()
    marker = re.escape(rows)
    pattern = re.compile(rf"(<!-- bench-table:{marker} -->\n)(?:.*?\n)?(<!-- /bench-table:{marker} -->)", re.S)
    if not pattern.search(text):
        sys.exit(f"no bench-table:{rows} markers in {page}")
    page.write_text(pattern.sub(lambda match: match.group(1) + table + "\n" + match.group(2), text, count=1))
    print(f"wrote the {rows} table into {page.relative_to(root)}", file=sys.stderr)


main()
