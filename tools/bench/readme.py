#!/usr/bin/env python3
"""Writes the benchmark blocks of README.md and editors/code/README.md from the committed results
files, `tools/bench/real/results.json` and `tools/bench/why/results.json`. GitHub and the VS Code
Marketplace show a README as it is, so these blocks are generated text, and `--check` fails when a
README differs from what this script would write.

The rules match `docs/src/lib/benchmarks.ts`, which renders the same files on the site: a program
that GNU R finishes in under 0.1 s is too short and left out of every geometric mean, only rows of
kind `workload` count as application workloads, and a ratio has two decimals below 10x and none from
10x up.

Usage: tools/bench/readme.py [--check]
"""
import json, math, pathlib, re, statistics, sys

root = pathlib.Path(__file__).resolve().parent.parent.parent
real_results = root / "tools/bench/real/results.json"
kernel_results = root / "tools/bench/why/results.json"
main_readme = root / "README.md"
extension_readme = root / "editors/code/README.md"
# The largest gain, a typical loop, whole-vector arithmetic, which gains little, and a typed program
# that loses.
readme_programs = ["fib", "drawdown", "vector_arith", "orders"]
too_short_seconds = 0.1

def main():
    check = sys.argv[1:] == ["--check"]
    if sys.argv[1:] not in ([], ["--check"]):
        sys.exit("usage: tools/bench/readme.py [--check]")
    real = json.loads(real_results.read_text())
    kernels = json.loads(kernel_results.read_text())
    blocks = {
        main_readme: {"bench-summary": summary_block(real, kernels), "bench-why": kernel_block(kernels)},
        extension_readme: {"bench-why": kernel_block(kernels)},
    }
    stale = []
    for readme, replacements in blocks.items():
        text = readme.read_text()
        updated = text
        for marker, block in replacements.items():
            updated, count = re.subn(
                rf"(<!-- {marker} -->\n).*?(<!-- /{marker} -->)",
                lambda match: match[1] + block + "\n" + match[2],
                updated,
                flags=re.S,
            )
            if count != 1:
                sys.exit(f"error: {readme} has no <!-- {marker} --> block to rewrite")
        if updated == text:
            continue
        if check:
            stale.append(readme)
        else:
            readme.write_text(updated)
            print(f"wrote {readme}")
    if stale:
        names = ", ".join(str(readme.relative_to(root)) for readme in stale)
        sys.exit(f"error: the benchmark blocks of {names} differ from the results files; run `just bench-readme`")

def summary_block(real, kernels):
    workloads = [row for row in real["workloads"] if row.get("kind", "workload") == "workload"]
    timed_real = [row for row in workloads if row["gnu_r"] >= too_short_seconds]
    timed_kernels = [row for row in kernels["programs"] if row["gnu_r"] >= too_short_seconds]
    # The same rows as `benchmarkSummaryRows` in docs/src/lib/benchmarks.ts.
    return "\n".join([
        "| Suite | What it is | ry vs GNU R | ry typed |",
        "| --- | --- | ---: | ---: |",
        f"| Numeric kernels (ours) | {len(kernels['programs'])} small programs we wrote: loops, recursion and vector code"
        f" | {ratio(geomean(row['gnu_r'] / row['ry'] for row in timed_kernels))}"
        f" | {ratio(geomean(row['gnu_r'] / row['ry_typed'] for row in timed_kernels))} |",
        f"| Application workloads (ours) | {len(workloads)} call-heavy programs we wrote: conditions, S3 and S4 objects,"
        f" records, validation, simulation | {ratio(geomean(row['gnu_r'] / row['ry_jit'] for row in timed_real))} | not typed |",
        "",
        f"Geometric means; programs that finish in under {too_short_seconds} s are left out of them.",
        "",
        "- Numeric kernels " + lowercase_first(metadata(kernels, "kernels")),
        "- Application workloads " + lowercase_first(metadata(real, "real")),
    ])

def kernel_block(kernels):
    by_name = {row["program"]: row for row in kernels["programs"]}
    lines = ["| program | ry | ry, typed |", "| --- | ---: | ---: |"]
    for name in readme_programs:
        row = by_name.get(name)
        if row is None:
            sys.exit(f"error: `{name}` is not in {kernel_results.relative_to(root)}; update `readme_programs`")
        if row["gnu_r"] < too_short_seconds:
            sys.exit(f"error: `{name}` is too short to show in the README; pick another program")
        lines.append(f"| `{name}` | {ratio(row['gnu_r'] / row['ry'])} | {ratio(row['gnu_r'] / row['ry_typed'])} |")
    return "\n".join(lines + ["", metadata(kernels, "kernels")])

def metadata(results, suite):
    """The same line `suiteMetadata` in docs/src/lib/benchmarks.ts prints under each chart."""
    version = re.search(r"\d+\.\d+\.\d+", results["r"])
    commit = f" at ry {results['commit']}" if "commit" in results else ""
    samples = f"{results['repeat']} rounds" if suite == "real" else f"the best of {results['repeat']} processes per engine"
    return (f"Measured on {results['date']}{commit} with GNU R {version[0] if version else results['r']} "
            f"on {results['machine']}; {samples}.")

def lowercase_first(text):
    return text[:1].lower() + text[1:]

def ratio(value):
    return f"{value:.0f}x" if value >= 10 else f"{value:.2f}x"

def geomean(ratios):
    return math.exp(statistics.fmean(math.log(value) for value in ratios))

if __name__ == "__main__":
    main()
