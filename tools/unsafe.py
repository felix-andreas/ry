#!/usr/bin/env python3
"""Keeps `unsafe` from growing outside the crates that own it.

    python3 tools/unsafe.py check      # fail when a file has more `unsafe` than its baseline
    python3 tools/unsafe.py baseline   # lower tools/unsafe-baseline.txt to the tree's counts
    python3 tools/unsafe.py stats      # `unsafe fn`, blocks and SAFETY comments per crate

`heap`, `libr`, `jit`, `typed` and `ssa` own the object model, the C API boundary and generated
code, so their `unsafe` is not counted. Every other crate reaches the heap through
`heap::handle`, and a file there may only lose `unsafe`: the baseline holds one count per file
(the `unsafe` keywords outside comments), a file over its count fails, and a file under it asks
for `baseline` so the lower count is locked in. `baseline` never raises a count, with one
exception: `crates/interp/src/handles.rs` holds the safe forms of the interpreter's raw helpers,
so it grows by a wrapper each time a migrated function needs one, and it may rise in a change
that lowers the total. Any other file that needs more says why in the same change, with its line
in the baseline edited by hand.
"""

import os
import re
import subprocess
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
BASELINE = os.path.join(ROOT, "tools", "unsafe-baseline.txt")
OWNERS = ("heap", "libr", "jit", "typed", "ssa")
WRAPPERS = "crates/interp/src/handles.rs"
HEADER = (
    "# `unsafe` keywords per file outside heap, libr, jit, typed and ssa, which may only shrink.\n"
    "# Lower: python3 tools/unsafe.py baseline\n"
)
KEYWORD = re.compile(r"\bunsafe\b")
FUNCTION = re.compile(r"\bunsafe\s+(?:extern\s+\"[^\"]*\"\s+)?fn\b")
BLOCK = re.compile(r"\bunsafe\s*\{")
SAFETY = re.compile(r"//.*\bSAFETY\b|#\s*Safety\b")


def main():
    command = sys.argv[1] if len(sys.argv) > 1 else "check"
    if command == "stats":
        stats()
    elif command == "baseline":
        write_baseline(counted())
    elif command == "check":
        sys.exit(check(counted()))
    else:
        sys.exit(f"unknown command {command!r}: expected check, baseline or stats")


def check(counts):
    baseline = read_baseline()
    falling = sum(counts.values()) < sum(baseline.values())
    over = [
        (path, count, baseline.get(path, 0))
        for path, count in sorted(counts.items())
        if count > baseline.get(path, 0) and not (path == WRAPPERS and falling)
    ]
    under = [path for path, limit in baseline.items() if counts.get(path, 0) != limit]
    for path, count, limit in over:
        print(f"{path}: {count} `unsafe`, baseline {limit}")
    if over:
        print(
            "\nReach the heap through `heap::handle` (development/design/memory-safety.md)"
            " instead of adding `unsafe` here."
        )
        return 1
    if under:
        print(
            f"{len(under)} file(s) differ from the baseline in a way it allows;"
            " run `python3 tools/unsafe.py baseline` to lock that in."
        )
        return 1
    return 0


def write_baseline(counts):
    baseline = read_baseline() if os.path.exists(BASELINE) else None
    falling = baseline is not None and sum(counts.values()) < sum(baseline.values())
    lines = []
    for path, count in sorted(counts.items()):
        if baseline is None or (path == WRAPPERS and falling):
            limit = count
        else:
            limit = min(count, baseline.get(path, 0))
        if limit:
            lines.append(f"{limit} {path}\n")
    with open(BASELINE, "w", encoding="utf-8") as file:
        file.write(HEADER + "".join(lines))
    print(f"{sum(int(line.split()[0]) for line in lines)} `unsafe` in {len(lines)} file(s)")


def read_baseline():
    baseline = {}
    with open(BASELINE, encoding="utf-8") as file:
        for line in file:
            if line.strip() and not line.startswith("#"):
                count, path = line.split(maxsplit=1)
                baseline[path.strip()] = int(count)
    return baseline


def counted():
    """`unsafe` keywords per source file of every crate that does not own `unsafe`."""
    counts = {}
    for path in sources():
        if crate_of(path) not in OWNERS:
            keywords = sum(len(KEYWORD.findall(line)) for line in code_lines(path))
            if keywords:
                counts[path] = keywords
    return counts


def stats():
    rows = {}
    for path in sources():
        row = rows.setdefault(crate_of(path), [0, 0, 0, 0])
        with open(os.path.join(ROOT, path), encoding="utf-8") as file:
            text = file.read()
        row[0] += text.count("\n")
        row[3] += sum(1 for line in text.splitlines() if SAFETY.search(line))
        for line in code_lines(path):
            row[1] += len(FUNCTION.findall(line))
            row[2] += len(BLOCK.findall(line))
    print(f"{'crate':<12}{'lines':>9}{'unsafe fn':>11}{'blocks':>8}{'SAFETY':>8}")
    for crate, (lines, functions, blocks, safety) in sorted(rows.items()):
        if functions or blocks:
            print(f"{crate:<12}{lines:>9}{functions:>11}{blocks:>8}{safety:>8}")


def sources():
    listed = subprocess.run(
        ["git", "ls-files", "--cached", "--others", "--exclude-standard", "crates/*/src/**.rs"],
        cwd=ROOT,
        check=True,
        capture_output=True,
        text=True,
    ).stdout.split("\n")
    return [path for path in listed if path and os.path.exists(os.path.join(ROOT, path))]


def crate_of(path):
    return path.split("/")[1]


def code_lines(path):
    """The file's lines with `//` comments and string literals blanked."""
    with open(os.path.join(ROOT, path), encoding="utf-8") as file:
        for line in file:
            line = re.sub(r'"(?:[^"\\]|\\.)*"', '""', line)
            yield line.split("//", 1)[0]


if __name__ == "__main__":
    main()
