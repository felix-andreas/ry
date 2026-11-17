#!/usr/bin/env python3
"""Keeps `unsafe` from growing outside the crates that own it.

    python3 tools/unsafe.py check      # fail when a file's `unsafe` count differs from its baseline
    python3 tools/unsafe.py baseline   # lower tools/unsafe-baseline.txt to the tree's counts
    python3 tools/unsafe.py stats      # `unsafe fn`, blocks and SAFETY comments per crate

`heap`, `libr`, `jit`, `typed` and `ssa` own the object model, the C API boundary and generated
code, so their `unsafe` is not counted. Every other crate reaches the heap through
`heap::handle`, and a file there may only lose `unsafe`: the baseline holds one count per file
(the `unsafe` keywords outside comments), a file over its count fails, and a file under it asks
for `baseline` so the lower count is locked in.

`baseline` only lowers counts. A count rises by a hand edit of its line, and `check` accepts a
line that is higher than a reference only when `.agents/memory/backlog.md` has a line for the
file that names the functions holding the new `unsafe` and allows at least that count:

    - `crates/interp/src/handles.rs` 107 to 149: `evaluate`, `force`, ...

The references are the baseline's first version and, on a branch, the baseline at the commit
the branch left main at, so the backlog names every `unsafe` above where a file started. They
need the history: CI fails without them, and elsewhere `check` says that raised counts went
unchecked.
"""

import os
import re
import subprocess
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
BASELINE = "tools/unsafe-baseline.txt"
BACKLOG = ".agents/memory/backlog.md"
OWNERS = ("heap", "libr", "jit", "typed", "ssa")
HEADER = (
    "# `unsafe` keywords per file outside heap, libr, jit, typed and ssa, which may only shrink.\n"
    "# Lower: python3 tools/unsafe.py baseline. A raise is a hand edit with a backlog line.\n"
)
KEYWORD = re.compile(r"\bunsafe\b")
# The ABI string of `unsafe extern "C" fn` is blank by the time this runs.
FUNCTION = re.compile(r"\bunsafe\s+(?:extern\s+)?fn\b")
BLOCK = re.compile(r"\bunsafe\s*\{")
SAFETY = re.compile(r"//.*\bSAFETY\b|#\s*Safety\b")


def main():
    command = sys.argv[1] if len(sys.argv) > 1 else "check"
    if command == "stats":
        stats()
    elif command == "baseline":
        sys.exit(lower_baseline(counted()))
    elif command == "check":
        sys.exit(check(counted()))
    else:
        sys.exit(f"unknown command {command!r}: expected check, baseline or stats")


def check(counts):
    baseline = read_baseline()
    over = report_over(counts, baseline)
    if over:
        return 1
    unexplained = unexplained_raises(baseline)
    if unexplained is None:
        print("note: no reference baseline in this checkout, so raised counts go unchecked")
        if os.environ.get("CI"):
            print("CI needs it: fetch the full history (`git fetch --unshallow`).")
            return 1
        unexplained = []
    for path, before, limit in unexplained:
        print(f"{path}: baseline raised from {before} to {limit} without a backlog line")
    if unexplained:
        print(
            f"\nA raised count needs a line in {BACKLOG} that names the functions:\n"
            "    - `PATH` BEFORE to AFTER: `function`, `function`, ..."
        )
        return 1
    under = [path for path, limit in baseline.items() if counts.get(path, 0) < limit]
    if under:
        print(
            f"{len(under)} file(s) have less `unsafe` than their baseline;"
            " run `python3 tools/unsafe.py baseline` to lock that in."
        )
        return 1
    return 0


def lower_baseline(counts):
    baseline = read_baseline()
    lines = []
    for path, limit in sorted(baseline.items()):
        lowered = min(limit, counts.get(path, 0))
        if lowered:
            lines.append(f"{lowered} {path}\n")
    with open(os.path.join(ROOT, BASELINE), "w", encoding="utf-8") as file:
        file.write(HEADER + "".join(lines))
    print(f"{sum(int(line.split()[0]) for line in lines)} `unsafe` in {len(lines)} file(s)")
    return 1 if report_over(counts, baseline) else 0


def report_over(counts, baseline):
    """Prints the files with more `unsafe` than their baseline, and says whether there are any."""
    over = [
        (path, count, baseline.get(path, 0))
        for path, count in sorted(counts.items())
        if count > baseline.get(path, 0)
    ]
    for path, count, limit in over:
        print(f"{path}: {count} `unsafe`, baseline {limit}")
    if over:
        print(
            "\nReach the heap through `heap::handle` (development/design/memory-safety.md)"
            " instead of adding `unsafe` here. `baseline` never raises a count."
        )
    return bool(over)


def unexplained_raises(baseline):
    """The baseline lines above a reference that no backlog line accounts for, or `None` when
    this checkout has no reference."""
    references = reference_baselines()
    if references is None:
        return None
    with open(os.path.join(ROOT, BACKLOG), encoding="utf-8") as file:
        backlog = file.read()
    raised = []
    for path, limit in sorted(baseline.items()):
        before = min(reference.get(path, 0) for reference in references)
        allowed = re.findall(rf"^\s*- `{re.escape(path)}` \d+ to (\d+): `\w", backlog, re.MULTILINE)
        if limit > before and not any(int(count) >= limit for count in allowed):
            raised.append((path, before, limit))
    return raised


def reference_baselines():
    """The baselines a count must not exceed without a backlog line: the file's first version,
    and on a branch the one at the commit it left main at. `None` without the first version."""
    added = git("log", "--diff-filter=A", "--format=%H %P", "--", BASELINE)
    first, *parents = added.strip().split("\n")[-1].split() if added and added.strip() else [None]
    # A shallow checkout shows its oldest commit as the one that adds every file.
    if not parents:
        return None
    commits = [first, git("merge-base", "HEAD", "origin/main")]
    return [baseline for baseline in map(baseline_at, commits) if baseline is not None]


def baseline_at(commit):
    text = commit and git("show", f"{commit.strip()}:{BASELINE}")
    return parse_baseline(text) if text else None


def git(*arguments):
    result = subprocess.run(["git", *arguments], cwd=ROOT, capture_output=True, text=True)
    return result.stdout if result.returncode == 0 else None


def read_baseline():
    with open(os.path.join(ROOT, BASELINE), encoding="utf-8") as file:
        return parse_baseline(file.read())


def parse_baseline(text):
    baseline = {}
    for line in text.split("\n"):
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
    """The file's lines with comments, string literals and character literals blanked."""
    with open(os.path.join(ROOT, path), encoding="utf-8") as file:
        return blank_literals(file.read()).split("\n")


RAW_STRING = re.compile(r'b?r(#*)"')
CHARACTER = re.compile(r"b?'(?:\\(?:x[0-9a-fA-F]{2}|u\{[0-9a-fA-F_]+\}|.)|[^\\'\n])'")


def blank_literals(text):
    """`text` with everything that is not code replaced by spaces, newlines kept.

    A lexer and not a pattern per line, because a string literal can span lines, hold `//`, and
    be raw, and a `'"'` character literal would otherwise open a string.
    """
    out = []
    position = 0
    end = len(text)

    def skip(to):
        nonlocal position
        out.append("".join("\n" if character == "\n" else " " for character in text[position:to]))
        position = to

    while position < end:
        character = text[position]
        if text.startswith("//", position):
            newline = text.find("\n", position)
            skip(end if newline < 0 else newline)
        elif text.startswith("/*", position):
            depth, cursor = 1, position + 2
            while depth and cursor < end:
                if text.startswith("/*", cursor):
                    depth, cursor = depth + 1, cursor + 2
                elif text.startswith("*/", cursor):
                    depth, cursor = depth - 1, cursor + 2
                else:
                    cursor += 1
            skip(cursor)
        elif (raw := RAW_STRING.match(text, position)) and not is_identifier_tail(text, position):
            close = text.find('"' + raw.group(1), raw.end())
            skip(end if close < 0 else close + 1 + len(raw.group(1)))
        elif character == '"':
            cursor = position + 1
            while cursor < end and text[cursor] != '"':
                cursor += 2 if text[cursor] == "\\" else 1
            skip(min(cursor + 1, end))
        elif (literal := CHARACTER.match(text, position)) and not is_identifier_tail(text, position):
            skip(literal.end())
        else:
            out.append(character)
            position += 1
    return "".join(out)


def is_identifier_tail(text, position):
    """Whether the `b` or `r` at `position` ends an identifier instead of starting a literal."""
    return position > 0 and (text[position - 1].isalnum() or text[position - 1] == "_") and text[position] != "'"


if __name__ == "__main__":
    main()
