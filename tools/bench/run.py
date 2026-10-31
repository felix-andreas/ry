#!/usr/bin/env python3
"""Benchmark harness: generate the three synthetic code bases, check them, and print a table.

For each code base it reports the cold `ry check` wall time and peak memory, the number of
findings by diagnostic code (the generated R is valid, so every error is a candidate false
positive), and the language server's re-check latency after a body edit and a signature edit.

Usage:
    python3 -I tools/bench/run.py [--ry target/release/ry] [--out DIR] [--lines 300000]
                                  [--files 500] [--runs 10] [--skip-lsp] [--json FILE]

Build the binary first with `cargo build --release -p ry-lang`; `just bench` does both.
"""

import argparse
import collections
import json
import os
import resource
import subprocess
import sys
import tempfile
import time

HERE = os.path.dirname(os.path.abspath(__file__))
DOMAINS = {"finance": "finbench", "datascience": "dsbench", "biostatistics": "biobench"}


def generate(out, lines, files):
    subprocess.run(
        [sys.executable, "-I", os.path.join(HERE, "generate.py"), "--out", out, "--lines", str(lines), "--files", str(files)],
        check=True,
        stdout=subprocess.DEVNULL,
    )
    for package in DOMAINS.values():
        # Type errors are opt-in, and the benchmark exists to exercise the type checker.
        with open(os.path.join(out, package, "ry.toml"), "w", encoding="utf-8") as handle:
            handle.write("[check]\ntyping = true\n")


def count_lines(root):
    total = 0
    for directory, _, names in os.walk(root):
        for name in names:
            if name.endswith(".R"):
                with open(os.path.join(directory, name), "rb") as handle:
                    total += handle.read().count(b"\n")
    return total


def cold_check(binary, root):
    """Wall time, peak resident set of the child alone, and findings by code."""
    before = resource.getrusage(resource.RUSAGE_CHILDREN).ru_maxrss
    with tempfile.TemporaryFile() as findings:
        started = time.perf_counter()
        subprocess.run([binary, "check", root, "--output", "json"], stdout=findings, stderr=subprocess.DEVNULL)
        elapsed = time.perf_counter() - started
        findings.seek(0)
        codes = collections.Counter(json.loads(line)["code"] for line in findings if line.strip())
    # ru_maxrss of children is a running maximum, so a smaller child cannot lower it; each
    # code base is therefore measured in its own subprocess below when exact numbers matter.
    peak = max(resource.getrusage(resource.RUSAGE_CHILDREN).ru_maxrss, before) / 1024
    return elapsed, peak, codes


def lsp_latency(binary, root, runs):
    result = subprocess.run(
        [sys.executable, "-I", os.path.join(HERE, "lsp_latency.py"), "--ry", binary, "--root", root, "--runs", str(runs), "--json"],
        capture_output=True,
        text=True,
    )
    if result.returncode != 0:
        return None
    return json.loads(result.stdout)


def isolated_cold_check(binary, root):
    """Re-run in a fresh interpreter so the child's peak is not inherited from a larger one."""
    script = (
        "import json,sys;sys.path.insert(0,sys.argv[1]);import run;"
        "e,p,c=run.cold_check(sys.argv[2],sys.argv[3]);print(json.dumps([e,p,c]))"
    )
    output = subprocess.run(
        [sys.executable, "-c", script, HERE, binary, root], capture_output=True, text=True, check=True
    ).stdout
    elapsed, peak, codes = json.loads(output)
    return elapsed, peak, collections.Counter(codes)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--ry", default=os.path.join(HERE, "..", "..", "target", "release", "ry"))
    parser.add_argument("--out", help="where the code bases go (default: a temporary directory)")
    parser.add_argument("--lines", type=int, default=300_000)
    parser.add_argument("--files", type=int, default=500)
    parser.add_argument("--runs", type=int, default=10)
    parser.add_argument("--skip-lsp", action="store_true")
    parser.add_argument("--json", help="also write the raw numbers here")
    args = parser.parse_args()

    binary = os.path.abspath(args.ry)
    if not os.path.exists(binary):
        sys.exit(f"{binary} does not exist; run `cargo build --release -p ry-lang` first")
    out = os.path.abspath(args.out) if args.out else tempfile.mkdtemp(prefix="ry-bench-")
    generate(out, args.lines, args.files)

    rows = []
    for domain, package in DOMAINS.items():
        root = os.path.join(out, package)
        elapsed, peak, codes = isolated_cold_check(binary, root)
        row = {
            "domain": domain,
            "lines": count_lines(root),
            "cold_s": elapsed,
            "peak_mib": peak,
            "findings": dict(codes),
        }
        if not args.skip_lsp:
            row["lsp"] = lsp_latency(binary, root, args.runs)
        rows.append(row)

    print(f"\ncode bases in {out}\n")
    print("| code base | lines | cold check | peak RSS | findings | body edit | signature edit |")
    print("|---|---:|---:|---:|---|---:|---:|")
    for row in rows:
        findings = ", ".join(f"{count} {code}" for code, count in sorted(row["findings"].items())) or "none"
        lsp = row.get("lsp")
        body = signature = "n/a"
        if lsp:
            body = f"{lsp['body']['first_p50_ms']:.0f} / {lsp['body']['settled_p50_ms']:.0f} ms"
            signature = f"{lsp['signature']['first_p50_ms']:.0f} / {lsp['signature']['settled_p50_ms']:.0f} ms"
        print(
            f"| {row['domain']} | {row['lines']:,} | {row['cold_s']:.1f} s | {row['peak_mib']:.0f} MiB "
            f"| {findings} | {body} | {signature} |"
        )
    print("\nEdit columns are p50 latency to the first publish / to the settled publish.")
    if args.json:
        with open(args.json, "w", encoding="utf-8") as handle:
            json.dump(rows, handle, indent=2)


if __name__ == "__main__":
    main()
