"""Compares base R kernels between GNU R and ry's runtime.

Runs `kernels.R` under `Rscript` and under `ry run --runtime ry` at each row count, keeps the
faster of the two runs per question, checks that both engines produce the same checksum, and
prints one markdown table per row count with the ratio ry / GNU R. With `--memory`, every
question runs in its own process instead, and the table adds each engine's peak resident set
size for that question.

Usage: python3 -I tools/bench/base/run.py [--rows 1e5,1e6,1e7] [--only REGEX] [--memory]
"""

import argparse
import os
import re
import subprocess
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(os.path.dirname(os.path.dirname(HERE)))
SCRIPT = os.path.join(HERE, "kernels.R")


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--rows", default="1e5,1e6,1e7")
    parser.add_argument("--only", default=None, help="regular expression picking questions")
    parser.add_argument("--memory", action="store_true", help="one process per question, with peak RSS")
    parser.add_argument("--ry", default=os.path.join(ROOT, "target", "release", "ry"))
    arguments = parser.parse_args()
    engines = {
        "gnu": ["Rscript", SCRIPT],
        "ry": [arguments.ry, "run", "--runtime", "ry", SCRIPT],
    }
    for rows in arguments.rows.split(","):
        if arguments.memory:
            results = {engine: {} for engine in engines}
            for question in questions(arguments.only):
                pattern = "^" + re.escape(question) + "$"
                for engine, command in engines.items():
                    results[engine].update(run(command, rows, pattern))
        else:
            results = {engine: run(command, rows, arguments.only) for engine, command in engines.items()}
        print_table(rows, results, arguments.memory)


def questions(only):
    names = re.findall(r'^time_question\("([^"]+)"', open(SCRIPT).read(), re.MULTILINE)
    return [name for name in names if only is None or re.search(only, name)]


def run(command, rows, only):
    """Runs one engine and returns {question: (best seconds, checksums, peak kB or None)}."""
    arguments = command + [rows] + ([only] if only else [])
    completed = subprocess.run(arguments, capture_output=True, text=True)
    if completed.returncode != 0:
        sys.stderr.write(completed.stdout + completed.stderr)
        raise SystemExit(f"{' '.join(arguments)} failed")
    times, checksums, peak = {}, {}, None
    for line in completed.stdout.splitlines():
        if line.startswith("#peak "):
            peak = int(line.split()[1])
            continue
        fields = line.split("\t")
        if len(fields) != 6:
            continue
        question, seconds, checksum = fields[0], float(fields[2]), fields[5]
        times[question] = min(times.get(question, seconds), seconds)
        checksums.setdefault(question, set()).add(checksum)
    return {question: (times[question], checksums[question], peak) for question in times}


def print_table(rows, results, memory):
    gnu, ry = results["gnu"], results["ry"]
    print(f"\n## {int(float(rows)):,} rows\n")
    header = "| question | GNU R (s) | ry (s) | ratio | checksum |"
    rule = "|---|---:|---:|---:|---|"
    if memory:
        header += " GNU R peak (MB) | ry peak (MB) | ratio |"
        rule += "---:|---:|---:|"
    print(header)
    print(rule)
    for question, (gnu_seconds, gnu_checksums, gnu_peak) in gnu.items():
        if question not in ry:
            print(f"| {question} | {gnu_seconds:.3f} | missing | | |")
            continue
        ry_seconds, ry_checksums, ry_peak = ry[question]
        ratio = ry_seconds / gnu_seconds if gnu_seconds > 0 else (1.0 if ry_seconds == 0 else float("inf"))
        status = "ok" if gnu_checksums == ry_checksums else "MISMATCH"
        line = f"| {question} | {gnu_seconds:.3f} | {ry_seconds:.3f} | {ratio:.2f} | {status} |"
        if memory:
            line += f" {gnu_peak / 1024:.0f} | {ry_peak / 1024:.0f} | {ry_peak / gnu_peak:.2f} |"
        print(line)


if __name__ == "__main__":
    main()
