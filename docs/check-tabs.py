"""Checks that the Inline and Comments tabs of each <Tabs> group in the given docs pages give the same diagnostics. Usage: RY=target/debug/ry python3 docs/check-tabs.py docs/src/content/docs/**/*.mdx (V=1 prints the outputs)."""
import re, subprocess, sys, tempfile, os, pathlib
RY = os.path.abspath(os.environ.get("RY", "target/debug/ry"))
def groups(text):
    for m in re.finditer(r"<Tabs[^>]*>(.*?)</Tabs>", text, re.S):
        blocks = re.findall(r"```r\n(.*?)```", m.group(1), re.S)
        yield m.start(), blocks
def run(code, strict):
    with tempfile.TemporaryDirectory() as d:
        pathlib.Path(d, "ry.toml").write_text("[check]\ntyping = true\n" + ("strict = true\n" if strict else ""))
        os.mkdir(d + "/R")
        pathlib.Path(d, "R/a.R").write_text(code)
        r = subprocess.run([RY, "check"], cwd=d, capture_output=True, text=True)
        return r.stdout + r.stderr
def norm(out):
    keep = []
    for line in out.splitlines():
        if re.match(r"\s*--\[", line) or re.match(r"\s*\d+ \|", line) or re.match(r"\s*\|", line): continue
        keep.append(line.strip())
    return "\n".join(l for l in keep if l)
bad = 0
for f in sys.argv[1:]:
    text = open(f).read()
    for pos, blocks in groups(text):
        line = text[:pos].count("\n") + 1
        if len(blocks) != 2: print(f"{f}:{line}: expected 2 blocks, got {len(blocks)}"); bad += 1; continue
        strict = "strict-mode" in f
        a, b = run(blocks[0], strict), run(blocks[1], strict)
        same = norm(a) == norm(b)
        print(f"{f}:{line}: {'same' if same else 'DIFFERENT'}")
        if not same or os.environ.get("V"):
            bad += not same
            print("--- inline\n" + a + "--- comments\n" + b)
sys.exit(bad)
