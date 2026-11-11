"""Checks that this machine can build, test, run the oracle and benchmark ry: `just doctor`.

Each line is `ok`, `warn` or `FAIL`, and every problem names the command that fixes it. It exits 1
when a check fails, so a script can gate on it. tools/dev-setup.sh provisions what it checks.

The freshness checks recompute the runtime identity (crates/libr/build_identity.rs): a hash of the
sources libR links, which `ry`, `libR.so` and the start-up image each embed. A binary that does not
carry the identity of the working tree was built from other sources.
"""

import mmap
import os
import pathlib
import re
import shutil
import subprocess
import sys

root = pathlib.Path(__file__).resolve().parent.parent
# The oldest release that parses the justfile; tools/dev-setup.sh installs a pinned newer one.
JUST_MINIMUM = (1, 40, 0)
# Every package an `# oracle: requires` line in crates/oracle/cases names.
ORACLE_PACKAGES = ["data.table", "bit64", "rlang", "knitr", "rmarkdown", "ragg", "lattice", "R6", "withr", "xts"]
# Pinned with FAST_PATHS in crates/interp/src/package_fast_paths.rs.
RLANG_VERSION = "1.3.0"
DISK_WARN_GB = 10

failures = 0


def main():
    print(f"ry doctor: {root}")
    check_just()
    check_rust()
    r = check_gnu_r()
    if r:
        check_oracle_packages(r)
    check_corpus()
    check_binaries()
    check_disk()
    sys.exit(1 if failures else 0)


def report(status, subject, detail, fix=None):
    global failures
    if status == "FAIL":
        failures += 1
    line = f"{status:>4}  {subject}: {detail}"
    if fix:
        line += f"\n      fix: {fix}"
    print(line)


def run(command, **kwargs):
    try:
        result = subprocess.run(command, capture_output=True, text=True, timeout=120, **kwargs)
    except (FileNotFoundError, subprocess.TimeoutExpired) as error:
        return None, str(error)
    return result.returncode, (result.stdout + result.stderr).strip()


def check_just():
    code, output = run(["just", "--version"])
    if code != 0:
        report("FAIL", "just", "not installed", "sudo tools/dev-setup.sh")
        return
    match = re.search(r"(\d+)\.(\d+)\.(\d+)", output)
    version = tuple(int(part) for part in match.groups()) if match else (0, 0, 0)
    minimum = ".".join(map(str, JUST_MINIMUM))
    if version < JUST_MINIMUM:
        report("FAIL", "just", f"{output} cannot parse the justfile (needs {minimum})", "sudo tools/dev-setup.sh")
    else:
        report("ok", "just", output)


def check_rust():
    toolchain = (root / "rust-toolchain.toml").read_text()
    channel = re.search(r'channel\s*=\s*"([^"]+)"', toolchain).group(1)
    code, output = run(["rustc", "--version"], cwd=root)
    if code != 0:
        report("FAIL", "rust", f"rustc does not run: {output}", "install rustup; it reads rust-toolchain.toml")
        return
    # A nightly's rustc carries the date of its last commit, the day before the channel's date.
    pinned = channel.removeprefix("nightly-")
    built = re.search(r"(\d{4}-\d{2}-\d{2})\)", output)
    if built and pinned != channel and built.group(1) not in (pinned, previous_day(pinned)):
        report("FAIL", "rust", f"{output} is not {channel} (rust-toolchain.toml)", "run cargo through rustup, or `nix develop`")
    else:
        report("ok", "rust", output)


def previous_day(date):
    import datetime

    return (datetime.date.fromisoformat(date) - datetime.timedelta(days=1)).isoformat()


def check_gnu_r():
    """The reference GNU R of crates/oracle/cases/gnu-r.txt, as ORACLE_R or the first R on PATH."""
    wanted = (root / "tools/base-r/VERSION").read_text().strip()
    recorded = dict(
        line.split(" ", 1) for line in (root / "crates/oracle/cases/gnu-r.txt").read_text().splitlines() if line and not line.startswith("#")
    )
    r = os.environ.get("ORACLE_R") or shutil.which("R")
    if not r:
        report("FAIL", "GNU R", f"no R on PATH and ORACLE_R is unset; the oracle needs {wanted}", "sudo tools/dev-setup.sh")
        return None
    script = (
        'cat(format(getRversion()), capabilities("profmem"), capabilities("ICU"), '
        'getOption("bitmapType"), basename(La_library()), basename(extSoftVersion()[["BLAS"]]), '
        'file.exists(file.path(R.home("lib"), "libR.so")), sep = "\\n")'
    )
    code, output = run([r, "--vanilla", "--slave", "-e", script])
    if code != 0:
        report("FAIL", "GNU R", f"{r} does not run: {output[-200:]}")
        return None
    version, profmem, icu, bitmap, lapack, blas, shlib = (output.splitlines() + [""] * 7)[:7]
    problems = []
    if version != wanted:
        problems.append(f"release {version}, not {wanted}")
    if profmem != "TRUE":
        problems.append("no memory profiling (utils__rprofmem and sys_call_shares_arguments differ)")
    if icu != "TRUE":
        problems.append("no ICU (collation and the string cases differ)")
    # A build without pango-cairo still reports capabilities("cairo"), but png() falls back to
    # bitmapType "Xlib", which fails headless with "X11 is not available".
    if bitmap != "cairo":
        problems.append(f'bitmapType "{bitmap}", not "cairo" (the graphics cases differ; build with pango-cairo)')
    if shlib != "TRUE":
        problems.append("no libR.so (ry's GNU R console and binary packages need --enable-R-shlib)")
    if problems:
        report("FAIL", "GNU R", f"{r}: " + "; ".join(problems), f"sudo tools/dev-setup.sh, then use /opt/R/{wanted}/bin/R")
        return r
    expected_lapack = recorded.get("lapack", "").split(" ")[0]
    if expected_lapack and lapack != expected_lapack:
        report("warn", "GNU R", f"{r} {version} uses LAPACK {lapack}, the recordings {expected_lapack}; the last digits of linear algebra may differ")
    else:
        report("ok", "GNU R", f"{r} {version}, memory profiling, ICU, cairo bitmaps, {blas} and {lapack}")
    return r


def check_oracle_packages(r):
    script = (
        "ip <- installed.packages(); "
        f"for (p in c({', '.join(repr(p) for p in ORACLE_PACKAGES)})) "
        'cat(p, if (p %in% rownames(ip)) ip[p, "Version"] else "-", "\\n")'
    )
    code, output = run([r, "--vanilla", "--slave", "-e", script], env={**os.environ})
    if code != 0:
        report("FAIL", "oracle packages", output[-200:])
        return
    versions = dict(line.split() for line in output.splitlines() if len(line.split()) == 2)
    missing = [package for package in ORACLE_PACKAGES if versions.get(package, "-") == "-"]
    if missing:
        report("FAIL", "oracle packages", f"missing from {r}'s library: {', '.join(missing)} (their cases skip)", "sudo tools/dev-setup.sh")
    elif versions["rlang"] != RLANG_VERSION:
        report("warn", "oracle packages", f"rlang {versions['rlang']}, not {RLANG_VERSION}: the rlang cases and trace_back fuzzing skip", "sudo tools/dev-setup.sh")
    else:
        report("ok", "oracle packages", ", ".join(f"{p} {versions[p]}" for p in ORACLE_PACKAGES))


def check_corpus():
    corpus = pathlib.Path(os.environ.get("RY_CORPUS_DIR", root / ".corpus")) / "packages"
    lock = root / "tools/bench/packages/lock.tsv"
    pinned = {line.split("\t")[0] for line in lock.read_text().splitlines()[1:] if line.strip()}
    library = corpus / "library"
    installed = {path.name for path in library.iterdir()} if library.is_dir() else set()
    missing = sorted(pinned - installed)
    if not installed:
        report("warn", "corpus", f"no package library at {library}: `just bench packages`, package-tests and the oracle's package cases need it", "just bench-packages install")
    elif missing:
        report("warn", "corpus", f"{len(missing)} of {len(pinned)} pinned packages missing from {library}: {', '.join(missing[:6])}{' ...' if len(missing) > 6 else ''}", "just bench-packages install")
    else:
        report("ok", "corpus", f"{len(pinned)} pinned packages in {library}")


def check_binaries():
    identity = runtime_identity(root / "crates").encode()
    target = pathlib.Path(os.environ.get("CARGO_TARGET_DIR", root / "target"))
    found = False
    for profile in ["debug", "release", "profiling"]:
        directory = target / profile
        binaries = {name: directory / name for name in ["ry", "libR.so", "ry-rcmd"]}
        if not binaries["ry"].exists() and not binaries["libR.so"].exists():
            continue
        found = True
        flag = {"debug": "", "release": " --release"}.get(profile, f" --profile {profile}")
        stale = [name for name in ["ry", "libR.so"] if not contains(binaries[name], identity)]
        if not binaries["ry-rcmd"].exists():
            stale.append("ry-rcmd (missing)")
        if stale:
            report("warn", f"binaries ({profile})", f"{', '.join(stale)} not built from the working tree", f"cargo build{flag} -p ry-lang -p libr -p rcmd")
        else:
            report("ok", f"binaries ({profile})", "ry and libR.so match the working tree")
    if not found:
        report("warn", "binaries", f"nothing built in {target}", "cargo build -p ry-lang -p libr -p rcmd")
    image = target / "base-r/library/base/R/ry-start.image"
    if not (target / "base-r").is_dir():
        report("warn", "base R", f"no {target}/base-r: `ry run` falls back to R_HOME, and scenarios that hide GNU R skip", "just base-r dev (or release)")
    elif not image_written_by(image, identity):
        report("warn", "base R", "the start-up image was written by another runtime build, so every start is the slow one", "just start-image dev (or release)")
    else:
        report("ok", "base R", f"{target}/base-r with a start-up image of this runtime")


def runtime_identity(crates):
    """crates/libr/build_identity.rs, step for step."""
    names, pending = {"libr"}, ["libr"]
    while pending:
        manifest = (crates / pending.pop() / "Cargo.toml").read_text()
        linked = False
        for line in map(str.strip, manifest.splitlines()):
            if line.startswith("["):
                header = line[1:].rstrip("]")
                linked = header.endswith("dependencies") and not header.endswith("dev-dependencies")
            elif linked and 'path = "../' in line:
                name = line.split('path = "../', 1)[1].split('"', 1)[0]
                if name not in names:
                    names.add(name)
                    pending.append(name)
    files = []
    for name in names:
        for part in ["src", "include"]:
            directory = crates / name / part
            if directory.is_dir():
                files += [path for path in directory.rglob("*") if not path.is_dir()]
        if (crates / name / "build.rs").is_file():
            files.append(crates / name / "build.rs")
    hash = 0xCBF29CE484222325

    def fnv1a(hash, data):
        for byte in data:
            hash = ((hash ^ byte) * 0x100000001B3) & 0xFFFFFFFFFFFFFFFF
        return hash

    # `PathBuf` orders by component, which is not byte order: `a/x` sorts before `a-b`.
    for path in sorted(files, key=lambda path: path.parts):
        data = path.read_bytes()
        hash = fnv1a(hash, str(path.relative_to(crates)).encode())
        hash = fnv1a(hash, len(data).to_bytes(8, "little"))
        hash = fnv1a(hash, data)
    return f"{hash:016x}"


def contains(path, needle):
    # An interrupted link leaves an empty file, which mmap refuses.
    if not path.exists() or path.stat().st_size == 0:
        return False
    with open(path, "rb") as file, mmap.mmap(file.fileno(), 0, access=mmap.ACCESS_READ) as data:
        return data.find(needle) != -1


def image_written_by(path, identity):
    """crates/interp/src/image.rs `is_current`: the magic, a header length, then the identity."""
    try:
        with open(path, "rb") as file:
            prefix = file.read(24 + len(identity))
    except OSError:
        return False
    return prefix[:8] == b"RYSTART\x05" and int.from_bytes(prefix[16:24], "little") == len(identity) and prefix[24:] == identity


def check_disk():
    target = pathlib.Path(os.environ.get("CARGO_TARGET_DIR", root / "target"))
    free = shutil.disk_usage(root).free / 1e9
    sizes = []
    if target.is_dir():
        for profile in sorted(path for path in target.iterdir() if path.is_dir()):
            code, output = run(["du", "-s", "--block-size=1G", str(profile)])
            if code == 0 and int(output.split()[0]) >= 1:
                sizes.append(f"{profile.name} {output.split()[0]}G")
    detail = f"{free:.0f} GB free" + (f"; target: {', '.join(sizes)}" if sizes else "")
    if free < DISK_WARN_GB:
        report("warn", "disk", detail + ". A cloud container's quota is about 40 GB whatever df says, and a release link needs several", "just prune")
    else:
        report("ok", "disk", detail)


if __name__ == "__main__":
    main()
