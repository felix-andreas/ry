#!/usr/bin/env python3
"""Writes the benchmark summary block of README.md and editors/code/README.md from the committed
suites, each a directory `tools/bench/<suite>/` with a `suite.toml` and a `results.json`. GitHub and
the VS Code Marketplace show a README as it is, so the block is generated text, and `--check` fails
when a README differs from what this script would write.

The block is the summary table of the benchmarks page, which `docs/src/lib/benchmarks.ts` renders
from the same files: one row per measured suite in a fixed order, with the geometric means the
runner stored, and a ratio with two decimals below 10x and none from 10x up. A suite without a
results file has no row.

A vertical grouped bar chart of the same geometric means, in a light and a dark SVG under
`docs/public/`, is shown by the root README through `<picture>`. It has one group per suite with
bars for GNU R (always 1x), ry and, where the suite has a typed column, ry typed. `--check` also
fails when either SVG differs from what this script would write.

Usage: tools/bench/readme.py [--check]
"""
import json, pathlib, re, sys, tomllib
from decimal import ROUND_HALF_UP, Decimal

root = pathlib.Path(__file__).resolve().parent.parent.parent
bench = root / "tools/bench"
readmes = [root / "README.md", root / "editors/code/README.md"]
suite_order = ["public", "packages", "kernels", "applications"]
marker = "bench-summary"
chart_dir = root / "docs/public"
# Series colors of docs/src/components/SpeedChart.astro, with a neutral gray for the GNU R baseline.
themes = {
    "light": {"ink": "#1f2328", "muted": "#59636e", "rule": "#8b949e", "gnu_r": "#8b8f97", "ry": "#2a78d6", "ry_typed": "#eb6834"},
    "dark": {"ink": "#e6edf3", "muted": "#9198a1", "rule": "#6e7681", "gnu_r": "#7d838c", "ry": "#3987e5", "ry_typed": "#d95926"},
}

def main():
    check = sys.argv[1:] == ["--check"]
    if sys.argv[1:] not in ([], ["--check"]):
        sys.exit("usage: tools/bench/readme.py [--check]")
    suites = load_suites()
    block = summary_block(suites)
    stale = []
    for theme in themes:
        path = chart_dir / f"bench-summary-{theme}.svg"
        svg = chart_svg(suites, theme)
        if path.exists() and path.read_text() == svg:
            continue
        if check:
            stale.append(path)
        else:
            path.write_text(svg)
            print(f"wrote {path}")
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
        results = check_schema(json.loads(results_path.read_text()))
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
    lines += [measured_line(suites), ""]
    lines += mixed_machines(suites)
    lines += ["[Benchmarks](https://ry-lang.org/benchmarks) explains how each suite is built and measured, and shows every program, with the machine and ry commit of each suite."]
    return "\n".join(lines)

def measured_line(suites):
    """One sentence for all suites; a field that differs between suites is listed per suite."""
    metas = [suite["results"]["meta"] for suite in suites]
    dates = sorted({meta["date"][:10] for meta in metas})
    versions = sorted({meta["gnu_r"]["version"] for meta in metas})
    kernels = sorted({(meta["machine"].get("kernel") or "an unknown OS").split()[0] for meta in metas})
    rounds = sorted({meta["rounds"] for meta in metas})
    when = dates[0] if len(dates) == 1 else f"{dates[0]} to {dates[-1]}"
    count = str(rounds[0]) if len(rounds) == 1 else f"{rounds[0]} to {rounds[-1]}"
    return f"Measured on {when} against GNU R {' and '.join(versions)} on {' and '.join(kernels)}, {count} paired rounds per suite."

def mixed_machines(suites):
    """A warning, followed by a blank line, when the suites were measured on different machines
    (CPU model or kernel), because their ratios are then not comparable with each other."""
    machines = {}
    for suite in suites:
        machine = suite["results"]["meta"]["machine"]
        key = ", ".join(part for part in (machine["cpu"], machine.get("kernel")) if part)
        machines.setdefault(key, []).append(suite["name"])
    if len(machines) < 2:
        return []
    where = "; ".join(f"{' and '.join(names)} on {key}" for key, names in machines.items())
    return [f"**Warning:** the suites were measured on different machines ({where}), so their ratios are not directly comparable.", ""]

def metadata(results):
    """The line `suiteMetadata` in docs/src/lib/benchmarks.ts prints under each chart."""
    meta = results["meta"]
    # The first nine hex digits, keeping a marker such as `-dirty`, as `shortCommit` does.
    commit = " at ry " + re.sub(r"^([0-9a-f]{9})[0-9a-f]*", r"\1", meta["ry_commit"]) if meta.get("ry_commit") else ""
    machine = ", ".join([meta["machine"]["cpu"]]
                        + ([f"{meta['machine']['cores']} cores"] if meta["machine"].get("cores") else [])
                        + ([meta["machine"]["kernel"]] if meta["machine"].get("kernel") else []))
    return f"Measured on {meta['date'][:10]}{commit} with GNU R {meta['gnu_r']['version']} on {machine}; {meta['rounds']} paired rounds."

def chart_svg(suites, theme):
    """Vertical grouped bars, one group per suite, each bar labeled with its value."""
    colors = themes[theme]
    width, height = 760, 300
    left, right, top, bottom = 16, 16, 44, 44
    plot_h = height - top - bottom
    bar_w, gap, inner_gap = 44, 36, 6
    series = [("gnu_r", "GNU R"), ("ry", "ry"), ("ry_typed", "ry typed")]
    groups = []
    for suite in suites:
        geomean = suite["results"]["geomean"]
        values = [("gnu_r", 1.0)] + [(key, geomean[key]["median"]) for key in ("ry", "ry_typed") if key in geomean]
        groups.append((short_name(suite["name"]), values))
    peak = max(value for _, values in groups for _, value in values)
    ceiling = peak * 1.12
    y = lambda value: top + plot_h * (1 - value / ceiling)
    group_w = (width - left - right) / max(len(groups), 1)
    out = [f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {width} {height}" width="{width}" height="{height}" role="img" aria-labelledby="t d" font-family="-apple-system, \'Segoe UI\', Helvetica, Arial, sans-serif">']
    out.append('<title id="t">ry speed-up over GNU R by benchmark suite</title>')
    summary = "; ".join(f"{name}: " + ", ".join(f"{dict(series)[key]} {ratio(value)}" for key, value in values) for name, values in groups)
    out.append(f'<desc id="d">Geometric mean speed-up over GNU R. {escape(summary)}.</desc>')
    # Legend, top left.
    cursor = left
    for key, label in series:
        out.append(f'<rect x="{cursor}" y="12" width="12" height="12" rx="2" fill="{colors[key]}"/>')
        out.append(f'<text x="{cursor + 18}" y="22.5" font-size="13" fill="{colors["ink"]}">{label}</text>')
        cursor += 18 + 8 * len(label) + 18
    out.append(f'<text x="{width - right}" y="22.5" font-size="12" text-anchor="end" fill="{colors["muted"]}">Geometric mean speed-up over GNU R, higher is faster</text>')
    base = y(1.0)
    for index, (name, values) in enumerate(groups):
        span = len(values) * bar_w + (len(values) - 1) * inner_gap
        x = left + group_w * index + (group_w - span) / 2
        for key, value in values:
            top_y = y(value)
            out.append(f'<path d="M{x:.1f} {height - bottom}V{top_y + 4:.1f}a4 4 0 0 1 4-4h{bar_w - 8}a4 4 0 0 1 4 4V{height - bottom}z" fill="{colors[key]}"/>')
            out.append(f'<text x="{x + bar_w / 2:.1f}" y="{top_y - 6:.1f}" font-size="14" font-weight="600" text-anchor="middle" fill="{colors["ink"]}">{ratio(value)}</text>')
            x += bar_w + inner_gap
        out.append(f'<text x="{left + group_w * index + group_w / 2:.1f}" y="{height - bottom + 22}" font-size="13" text-anchor="middle" fill="{colors["ink"]}">{escape(name)}</text>')
    out.append(f'<line x1="{left}" x2="{width - right}" y1="{height - bottom}" y2="{height - bottom}" stroke="{colors["rule"]}" stroke-width="1"/>')
    out.append(f'<line x1="{left}" x2="{width - right}" y1="{base:.1f}" y2="{base:.1f}" stroke="{colors["rule"]}" stroke-width="1" stroke-dasharray="4 4"/>')
    out.append("</svg>")
    return "\n".join(out) + "\n"

def short_name(name):
    """Chart labels drop the `(ours)` marker, which the table keeps."""
    return re.sub(r"\s*\(ours\)", "", name)

def escape(text):
    return text.replace("&", "&amp;").replace("<", "&lt;").replace(">", "&gt;")

def ratio(value):
    # Rounds half up on the exact binary value, as JavaScript's toFixed does in `formatRatio`.
    places = Decimal(1) if value >= 10 else Decimal("0.01")
    return f"{Decimal(value).quantize(places, rounding=ROUND_HALF_UP)}x"

def check_schema(results):
    if results.get("schema") != 1:
        sys.exit(f"error: results.json has schema {results.get('schema')}, which this script does not read")
    return results

if __name__ == "__main__":
    main()
