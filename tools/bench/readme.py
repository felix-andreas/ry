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
import json, math, pathlib, re, statistics, sys, tomllib

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
        suites.append({**manifest, "results": results})
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
    commit = f" at ry {meta['ry_commit'][:9]}" if meta.get("ry_commit") else ""
    machine = ", ".join([meta["machine"]["cpu"]]
                        + ([f"{meta['machine']['cores']} cores"] if meta["machine"].get("cores") else [])
                        + ([meta["machine"]["kernel"]] if meta["machine"].get("kernel") else []))
    sampling = results.get("legacy_sampling") or f"{meta['rounds']} paired rounds"
    return f"Measured on {meta['date'][:10]}{commit} with GNU R {meta['gnu_r']['version']} on {machine}; {sampling}."

def lowercase_first(text):
    return text[:1].lower() + text[1:]

def ratio(value):
    return f"{value:.0f}x" if value >= 10 else f"{value:.2f}x"

def normalize(results):
    if results.get("schema") == 1:
        return results
    if "schema" not in results:
        return from_legacy(results)
    sys.exit(f"error: results.json has schema {results['schema']}, which this script does not read")

# TEMPORARY: maps the results files of `tools/bench/why` (numeric kernels) and `tools/bench/real`
# (application workloads), written before the schema-1 runner, into schema 1. Delete it, and
# `legacy_sampling` with it, once those directories are gone. `fromLegacy` in
# docs/src/lib/benchmarks.ts is its twin.
def from_legacy(legacy):
    kernels = "programs" in legacy
    if kernels:
        engines = ["gnu_r", "ry", "ry_typed"]
        rows = [{"id": row["program"], "seconds": {engine: row[engine] for engine in engines}} for row in legacy["programs"]]
    else:
        engines = ["gnu_r", "ry", "ry_nojit"]
        rows = [{"id": row["workload"], "seconds": {"gnu_r": row["gnu_r"], "ry": row["ry_jit"], "ry_nojit": row["ry_nojit"]}}
                for row in legacy["workloads"] if row.get("kind", "workload") == "workload"]
    # These files flagged a program by GNU R's time alone and left it out of every geometric mean.
    timed = [row for row in rows if row["seconds"]["gnu_r"] >= 0.1]
    geomean = {
        engine: {"median": math.exp(statistics.fmean(math.log(row["seconds"]["gnu_r"] / row["seconds"][engine]) for row in timed)),
                 "programs": len(timed)}
        for engine in engines[1:]
    }
    cpu, cores, kernel = (legacy["machine"].split(", ") + [None, None])[:3]
    version = re.search(r"\d+\.\d+\.\d+", legacy["r"])
    return {
        "schema": 1,
        "engines": engines,
        "meta": {
            "date": legacy["date"],
            "ry_commit": legacy.get("commit"),
            "gnu_r": {"version": version[0] if version else legacy["r"]},
            "machine": {"cpu": cpu, "cores": int(cores.split()[0]) if cores else None, "kernel": kernel},
            "rounds": legacy["repeat"],
        },
        "programs": rows,
        "geomean": geomean,
        "legacy_sampling": (f"each engine's best time over {legacy['repeat']} processes, run one engine after another" if kernels
                            else f"each engine's median time over {legacy['repeat']} rounds"),
    }

if __name__ == "__main__":
    main()
