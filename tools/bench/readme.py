#!/usr/bin/env python3
"""Writes the benchmark summary block of README.md and editors/code/README.md from the committed
suites, each a directory `tools/bench/<suite>/` with a `suite.toml` and a `results.json`. GitHub and
the VS Code Marketplace show a README as it is, so the block is generated text, and `--check` fails
when a README differs from what this script would write.

The block is the summary table of the benchmarks page, which `docs/src/lib/benchmarks.ts` renders
from the same files: one row per measured suite in a fixed order, with the geometric means the
runner stored, and a ratio with two decimals below 10x and none from 10x up. A suite without a
results file has no row.

Usage: tools/bench/readme.py [--check]
"""
import json, pathlib, re, sys, tomllib
from decimal import ROUND_HALF_UP, Decimal

root = pathlib.Path(__file__).resolve().parent.parent.parent
bench = root / "tools/bench"
readmes = [root / "README.md", root / "editors/code/README.md"]
suite_order = ["public", "packages", "kernels", "applications"]
marker = "bench-summary"

def main():
    check = sys.argv[1:] == ["--check"]
    if sys.argv[1:] not in ([], ["--check"]):
        sys.exit("usage: tools/bench/readme.py [--check]")
    block = summary_block(load_suites())
    stale = []
    for readme in readmes:
        text = readme.read_text()
        updated, count = re.subn(
            rf"(<!-- {marker} -->\n).*?(<!-- /{marker} -->)",
            lambda match: match[1] + block + "\n" + match[2],
            text,
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
        sys.exit(f"error: the benchmark block of {names} differs from the results files; run `just bench-readme`")

def load_suites():
    """The measured suites in page order, each its manifest with its results beside it."""
    suites = []
    for manifest_path in bench.glob("*/suite.toml"):
        results_path = manifest_path.with_name("results.json")
        if not results_path.exists():
            continue
        manifest = tomllib.loads(manifest_path.read_text())
        results = normalize(json.loads(results_path.read_text()))
        twin = next((suite for suite in suites if suite["id"] == manifest["id"]), None)
        if twin:
            sys.exit(f"error: benchmark suite `{manifest['id']}` is defined by both {twin['directory']} and {manifest_path.parent}")
        suites.append({**manifest, "directory": manifest_path.parent, "results": results})
    rank = lambda suite: (suite_order.index(suite["id"]) if suite["id"] in suite_order else len(suite_order), suite["id"])
    return sorted(suites, key=rank)

def summary_block(suites):
    """The rows of `summaryRows` and the lines of `summaryMarkdown` in docs/src/lib/benchmarks.ts."""
    lines = ["| Suite | What it is | ry vs GNU R | ry typed |", "| --- | --- | ---: | ---: |"]
    for suite in suites:
        geomean = suite["results"]["geomean"]
        untyped = ratio(geomean["ry"]["median"]) if "ry" in geomean else "not measured"
        typed = ratio(geomean["ry_typed"]["median"]) if "ry_typed" in geomean else "not typed"
        lines.append(f"| {suite['name']} | {len(suite['results']['programs'])} {suite['summary']} | {untyped} | {typed} |")
    lines += ["", "Geometric means of the speed-ups over GNU R; above 1x means ry is faster.", ""]
    lines += [f"- {suite['name']}: {lowercase_first(metadata(suite['results']))}" for suite in suites]
    lines += ["", "[Benchmarks](https://ry-lang.org/benchmarks) explains how each suite is built and measured, and shows every program."]
    return "\n".join(lines)

def metadata(results):
    """The line `suiteMetadata` in docs/src/lib/benchmarks.ts prints under each chart."""
    meta = results["meta"]
    # The first nine hex digits, keeping a marker such as `-dirty`, as `shortCommit` does.
    commit = " at ry " + re.sub(r"^([0-9a-f]{9})[0-9a-f]*", r"\1", meta["ry_commit"]) if meta.get("ry_commit") else ""
    machine = ", ".join([meta["machine"]["cpu"]]
                        + ([f"{meta['machine']['cores']} cores"] if meta["machine"].get("cores") else [])
                        + ([meta["machine"]["kernel"]] if meta["machine"].get("kernel") else []))
    return f"Measured on {meta['date'][:10]}{commit} with GNU R {meta['gnu_r']['version']} on {machine}; {meta['rounds']} paired rounds."

def lowercase_first(text):
    return text[:1].lower() + text[1:]

def ratio(value):
    # Rounds half up on the exact binary value, as JavaScript's toFixed does in `formatRatio`.
    places = Decimal(1) if value >= 10 else Decimal("0.01")
    return f"{Decimal(value).quantize(places, rounding=ROUND_HALF_UP)}x"

def normalize(results):
    if results.get("schema") == 1:
        return results
    sys.exit(f"error: results.json has schema {results['schema']}, which this script does not read")

if __name__ == "__main__":
    main()
