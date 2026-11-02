"""Times ry's standard library against the best GNU R equivalent of each function.

Every case in `ry.R` names one `List`, `Str`, `Dict` or `Result` function, and `gnu.R` holds the
same case written in base R, purrr, stringr and stringi. Each case runs in its own `ry run` and
`Rscript` process, the two side by side, so a pathologically slow case is cut off by `--timeout`
without stopping the others. The table has one row per function: the best ry time, the R alternative that was fastest
and its time, the ratio ry / R (above 1 means ry is slower), and whether the result checksums
agree. Packages load from R's usual library path, so set `R_LIBS_USER` to use another library;
an alternative whose package is missing is skipped, and a row with none left says which package.

Usage: python3 -I tools/bench/stdlib/run.py [--only SUBSTRING] [--n 1e6] [--sort ratio]
       [--ry target/release/ry] [--timeout 30] [--budget 1] [--write tools/bench/stdlib/results.md]
"""

import argparse
import datetime
import os
import re
import shlex
import subprocess
import sys
from concurrent.futures import ThreadPoolExecutor

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(os.path.dirname(os.path.dirname(HERE)))
HARNESS = os.path.join(HERE, "harness.R")
RY_CASES = os.path.join(HERE, "ry.R")
GNU_CASES = os.path.join(HERE, "gnu.R")


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--only", default="", help="run the functions whose name contains this substring")
    parser.add_argument("--n", default="1e6", help="collection size and call count")
    parser.add_argument("--sort", choices=["file", "ratio"], default="file", help="row order")
    parser.add_argument("--ry", default=os.path.join(ROOT, "target", "release", "ry"))
    parser.add_argument("--rscript", default="Rscript")
    parser.add_argument("--timeout", type=float, default=30.0, help="seconds per case and engine")
    parser.add_argument("--budget", type=float, default=1.0, help="stop repeating a case once its runs exceed this")
    parser.add_argument("--write", default=None, help="also write the table, with a header, to this file")
    arguments = parser.parse_args()

    names = [name for name in case_names(RY_CASES) if arguments.only in name]
    gnu_names = set(case_names(GNU_CASES))
    rows = []
    for name in names:
        # The two engines run side by side, each on its own core, which halves the wall time.
        with ThreadPoolExecutor(max_workers=2) as pool:
            ry_future = pool.submit(run_case, [arguments.ry, "run", RY_CASES], name, arguments)
            gnu_future = pool.submit(run_case, [arguments.rscript, GNU_CASES], name, arguments) if name in gnu_names else None
            ry = ry_future.result()
            gnu = gnu_future.result() if gnu_future else missing_case()
        row = make_row(name, ry, gnu)
        rows.append(row)
        sys.stderr.write(f"{name}: ry {row[1]}, {row[2]} {row[3]}\n")
    if arguments.sort == "ratio":
        rows.sort(key=lambda row: -row[6])
    table = render(rows)
    print(table)
    if arguments.write:
        command = "python3 -I tools/bench/stdlib/run.py " + " ".join(shlex.quote(argument) for argument in sys.argv[1:])
        header = (
            "# Standard library benchmark\n\n"
            f"Produced on {datetime.date.today().isoformat()} by `{command}`, with n = {arguments.n}.\n"
            "Each time is the best of up to three runs in seconds; the ratio is ry / R, so above 1 means ry is slower.\n"
            "`check` compares a checksum of both results (`differ` means they disagree, `n/a` that one side has no\n"
            "result). Rerun with `just bench-stdlib [filter] [n]`.\n\n"
        )
        with open(arguments.write, "w") as file:
            file.write(header + table + "\n")


def case_names(path):
    return re.findall(r'^case\("([^"]+)"', open(path).read(), re.MULTILINE)


def run_case(command, name, arguments):
    """Runs one case and returns (label, seconds or None, checksum, note)."""
    full = command + [HARNESS, name, arguments.n, str(arguments.budget)]
    try:
        completed = subprocess.run(full, capture_output=True, text=True, timeout=arguments.timeout)
    except subprocess.TimeoutExpired:
        return ("timeout", None, "", f"over {arguments.timeout:g}s")
    for line in completed.stdout.splitlines():
        fields = line.split("\t")
        if len(fields) == 5 and fields[0] == name:
            label, seconds, checksum, note = fields[1:]
            return (label, None if seconds == "NA" else float(seconds), checksum, note)
    message = (completed.stderr.strip().splitlines() or ["no output"])[-1]
    return ("error", None, "", message)


def missing_case():
    return ("none", None, "", "")


def make_row(name, ry, gnu):
    ry_label, ry_seconds, ry_checksum, ry_note = ry
    gnu_label, gnu_seconds, gnu_checksum, gnu_note = gnu
    if ry_seconds is not None and gnu_seconds is not None:
        ratio = ry_seconds / gnu_seconds if gnu_seconds > 0 else (1.0 if ry_seconds == 0 else float("inf"))
        check = "ok" if ry_checksum == gnu_checksum else "differ"
    else:
        ratio = float("-inf")
        check = "n/a"
    ry_cell = f"{ry_seconds:.4f}" if ry_seconds is not None else ry_label
    gnu_cell = f"{gnu_seconds:.4f}" if gnu_seconds is not None else ""
    notes = [note for note in (ry_note and f"ry: {ry_note}", gnu_note and f"R: {gnu_note}") if note]
    if check == "differ":
        notes.append(f"ry {ry_checksum} vs R {gnu_checksum}")
    ratio_cell = f"{ratio:.2f}" if ratio != float("-inf") else ""
    return (name, ry_cell, gnu_label, gnu_cell, ratio_cell, check, ratio, "; ".join(notes))


def render(rows):
    lines = ["| function | ry (s) | R equivalent | R (s) | ratio | check | notes |", "|---|---:|---|---:|---:|---|---|"]
    for name, ry_cell, gnu_label, gnu_cell, ratio_cell, check, _, notes in rows:
        notes = notes.replace("|", "\\|")
        lines.append(f"| {name} | {ry_cell} | `{gnu_label}` | {gnu_cell} | {ratio_cell} | {check} | {notes} |")
    return "\n".join(lines)


if __name__ == "__main__":
    main()
