#!/usr/bin/env python3
"""Writes the benchmark summary block of README.md and editors/code/README.md from the committed
suites, each a directory `tools/bench/<suite>/` with a `suite.toml` and a `results.json`. GitHub and
the VS Code Marketplace show a README as it is, so the block is generated text, and `--check` fails
when a README differs from what this script would write.

The block is one bullet per measured suite in a fixed order, giving the geometric means the runner
stored and what the suite measures, then one sentence on how they were measured. Ratios have two
decimals below 10x and none from 10x up, as on the benchmarks page, which
`docs/src/lib/benchmarks.ts` renders from the same files. A suite without a results file has no
bullet.

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
        sys.exit(f"error: the benchmark block of {names} differs from the results files; run `just readme`")

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
    """A bullet per suite, such as `**Numeric kernels: 4.89x, 11x in typed ry.** 16 small programs ...`."""
    lines = []
    for suite in suites:
        geomean = suite["results"]["geomean"]
        speeds = [ratio(geomean["ry"]["median"]) if "ry" in geomean else "not measured"]
        if "ry_typed" in geomean:
            speeds.append(f"{ratio(geomean['ry_typed']['median'])} in typed ry")
        lines.append(f"- **{short_name(suite['name'])}: {', '.join(speeds)}.** {len(suite['results']['programs'])} {suite['summary']}.")
    note = [measured_sentence(suites)] + mixed_machines(suites)
    note.append("[Benchmarks](https://ry-lang.org/benchmarks) shows every program and how it is measured.")
    return "\n".join(lines + ["", " ".join(note)])

def measured_sentence(suites):
    """One sentence for all suites; a field that differs between suites is given as a list or range."""
    metas = [suite["results"]["meta"] for suite in suites]
    versions = sorted({meta["gnu_r"]["version"] for meta in metas})
    kernels = sorted({(meta["machine"].get("kernel") or "an unknown OS").split()[0] for meta in metas})
    rounds = sorted({meta["rounds"] for meta in metas})
    count = str(rounds[0]) if len(rounds) == 1 else f"{rounds[0]} to {rounds[-1]}"
    return (f"Each number is the geometric-mean speed-up over GNU R {' and '.join(versions)} (above 1x is faster), "
            f"from {count} paired rounds on {' and '.join(kernels)}.")

def mixed_machines(suites):
    """A warning when the suites were measured on different machines (CPU model or kernel), because
    their ratios are then not comparable with each other; the benchmarks page names each machine."""
    machines = set()
    for suite in suites:
        machine = suite["results"]["meta"]["machine"]
        machines.add((machine["cpu"], machine.get("kernel")))
    if len(machines) < 2:
        return []
    return ["**Warning:** the suites ran on different machines, so their numbers are not directly comparable."]

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
    """Chart labels and bullets drop the `(ours)` marker, because the summaries say "we wrote"."""
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
