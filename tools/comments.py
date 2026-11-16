#!/usr/bin/env python3
"""Keeps Rust comments short, because agents read whole files and every comment line costs context.

    python3 tools/comments.py check      # fail when a file has more violations than its baseline
    python3 tools/comments.py report     # list the worst blocks and files, largest first
    python3 tools/comments.py baseline   # lower tools/comments-baseline.txt to the tree's counts
    python3 tools/comments.py stats      # comment bytes and lines per crate

A violation is one of:

- a `//` or `///` block longer than 12 lines, or a `//!` module doc longer than 25;
- a comment that names process history (a phase, a milestone, a commit hash, a pull request);
- a banner or separator comment such as `// -----` or `// === Section ===`;
- a section heading: a `//` line of at most six words between blank lines, such as `// Parsing.`.

The baseline holds per-file counts and may only shrink: a file over its count fails, and a file
under it asks for `baseline` so the lower count is locked in. A license header at the top of a
file and everything under `patches/` (vendored code) are exempt.
"""

import os
import re
import subprocess
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
BASELINE = os.path.join(ROOT, "tools", "comments-baseline.txt")
SKIP = ("patches/", "target/")
BLOCK_LIMIT = {"//": 12, "///": 12, "//!": 25}

PROCESS = re.compile(
    r"\b[Pp]hase \d|\bM\d\b|\bthe spike\b|\bthe cutover\b|\b[Pp]reviously\b|\bused to (?:be|panic|fail|ask|crash|return)\b"
    r"|\bpull request #?\d|(?<![\w&%#R])#\d{2,}\b|\b(?=[0-9a-f]*[a-f])(?=[0-9a-f]*\d)[0-9a-f]{7,40}\b(?![-+.]\d)"
)
BANNER = re.compile(r"^//[/!]?\s*(?:[-=*#~_/]{4,}|.*\s[-=*#~]{3,}\s*$)")
COMMENT = re.compile(r"^\s*(//!|///(?!/)|//)(.*)$")
SECTION = re.compile(r"^\s*// [A-Z][\w'-]*( [\w'`-]+){0,5}\.?$")


def main():
    command = sys.argv[1] if len(sys.argv) > 1 else "check"
    violations = scan()
    if command == "report":
        report(violations, int(sys.argv[2]) if len(sys.argv) > 2 else 60)
    elif command == "baseline":
        write_baseline(violations)
    elif command == "stats":
        stats()
    elif command == "check":
        sys.exit(check(violations))
    else:
        sys.exit(f"unknown command {command!r}: expected check, report, baseline or stats")


def rust_files():
    output = subprocess.run(
        ["git", "ls-files", "-z", "--", "*.rs"], cwd=ROOT, capture_output=True, check=True
    ).stdout.decode()
    return sorted(path for path in output.split("\0") if path and not path.startswith(SKIP))


def scan():
    """Returns {path: [(line, kind, size, text)]} for every violation in the tree."""
    found = {}
    for path in rust_files():
        with open(os.path.join(ROOT, path), encoding="utf-8", errors="replace") as file:
            lines = file.read().split("\n")
        hits = found.setdefault(path, [])
        block_start, block_kind, block_bytes = None, None, 0
        for number, line in enumerate(lines + [""], start=1):
            match = COMMENT.match(line)
            kind = match.group(1) if match else None
            if kind != block_kind:
                license = block_start == 1 and re.search(r"SPDX|Copyright", "\n".join(lines[:5]))
                if block_kind and number - block_start > BLOCK_LIMIT[block_kind] and not license:
                    hits.append((block_start, "long " + block_kind, number - block_start, block_bytes))
                block_start, block_kind, block_bytes = number, kind, 0
            if not match:
                continue
            block_bytes += len(line) + 1
            stripped = line.strip()
            if BANNER.match(stripped):
                hits.append((number, "banner", 1, len(line)))
            elif SECTION.match(line) and 1 < number < len(lines) and not (
                lines[number - 2].strip() or lines[number].strip()
            ):
                hits.append((number, "section", 1, len(line)))
            elif PROCESS.search(match.group(2)):
                hits.append((number, "process", 1, len(line)))
        if not hits:
            del found[path]
    return found


def check(violations):
    baseline = read_baseline()
    failed, shrunk = [], []
    for path in sorted(set(violations) | set(baseline)):
        count, allowed = len(violations.get(path, [])), baseline.get(path, 0)
        if count > allowed:
            failed.append(path)
            print(f"{path}: {count} comment violations, baseline allows {allowed}")
            for line, kind, size, _ in violations[path]:
                print(f"  {path}:{line}: {kind}" + (f" ({size} lines)" if kind.startswith("long") else ""))
        elif count < allowed:
            shrunk.append(path)
    if shrunk:
        print(f"{len(shrunk)} files are under their baseline: run `python3 tools/comments.py baseline`")
    if failed:
        print("Shorten the comments: keep the non-obvious reason, drop what restates the code or its history.")
        return 1
    return 1 if shrunk else 0


def report(violations, limit=60):
    rows = [(path, *hit) for path, hits in violations.items() for hit in hits]
    rows.sort(key=lambda row: -row[4])
    for path, line, kind, size, size_bytes in rows[:limit]:
        print(f"{size_bytes:>6} B  {size:>4} lines  {kind:<9} {path}:{line}")
    print(f"\n{len(rows)} violations in {len(violations)} files")
    per_file = sorted(violations.items(), key=lambda item: -sum(hit[3] for hit in item[1]))
    for path, hits in per_file[:20]:
        print(f"{sum(hit[3] for hit in hits):>7} B  {len(hits):>3}  {path}")


def stats():
    totals = {}
    for path in rust_files():
        group = "/".join(path.split("/")[:2]) if path.startswith("crates/") else path.split("/")[0]
        with open(os.path.join(ROOT, path), encoding="utf-8", errors="replace") as file:
            for line in file:
                entry = totals.setdefault(group, [0, 0, 0])
                entry[0] += len(line.encode())
                if COMMENT.match(line):
                    entry[1] += len(line.encode())
                    entry[2] += 1
    for group, (total, comment_bytes, comment_lines) in sorted(totals.items(), key=lambda item: -item[1][1]):
        print(f"{group:<24} {comment_bytes:>9} B {comment_lines:>7} lines {100 * comment_bytes / total:5.1f}%")
    total = sum(entry[0] for entry in totals.values())
    comment_bytes = sum(entry[1] for entry in totals.values())
    comment_lines = sum(entry[2] for entry in totals.values())
    print(f"{'total':<24} {comment_bytes:>9} B {comment_lines:>7} lines {100 * comment_bytes / total:5.1f}%")


def read_baseline():
    if not os.path.exists(BASELINE):
        return {}
    with open(BASELINE, encoding="utf-8") as file:
        rows = (line.split() for line in file if line.strip() and not line.startswith("#"))
        return {path: int(count) for count, path in rows}


def write_baseline(violations):
    baseline = read_baseline()
    grown = [path for path, hits in violations.items() if baseline and len(hits) > baseline.get(path, 0)]
    if grown and "--force" not in sys.argv:
        sys.exit("the baseline only shrinks, but these files have more violations: " + ", ".join(grown))
    with open(BASELINE, "w", encoding="utf-8") as file:
        file.write("# Comment violations per file, which may only shrink. Regenerate: python3 tools/comments.py baseline\n")
        for path in sorted(violations):
            file.write(f"{len(violations[path])} {path}\n")


if __name__ == "__main__":
    main()
