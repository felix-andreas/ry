#!/usr/bin/env python3
"""Times the test suites of real CRAN packages on GNU R and on ry's runtime.

The corpus is pinned: lock.tsv names every package, its version and the SHA-256 of its source
tarball, so two machines that run this script measure the same code. The sources come from the
rrepo CRAN mirror (https://rrepo.dev/upstream/cran), which serves every version CRAN ever
published. It is a REST API rather than a CRAN repository layout, so `install` downloads the pinned
tarballs into a local CRAN-like repository (.corpus/packages/repo) and installs from there with GNU
R into .corpus/packages/library. ry's runtime loads those same builds, since its libR is
ABI-compatible with GNU R's.

Subcommands:
  lock     Resolves packages.txt to the newest versions that support the installed GNU R, with
           their Depends, Imports and LinkingTo closure, and rewrites lock.tsv.
  install  Downloads and verifies the locked tarballs and installs them with GNU R.
  run      Runs each package's tests once per engine and repeat, alternating engines, and prints
           a table of wall times, outcomes and the geometric mean of GNU R's time over ry's.
           `--write` stores results.json beside this script.

A package's tests run the way R CMD check runs them: testthat suites through `test_dir()` on the
installed package, tinytest suites through `run_test_dir()`, and other packages' tests/*.R scripts
one by one. The time is the wall time of the whole process, so it includes start-up and loading
the package and its imports, as real use does.

Usage: tools/bench/packages/run.py lock | install [--jobs N] | run [--ry PATH] [--filter NAME]
                                   [--repeat N] [--write]
"""
import argparse, concurrent.futures, hashlib, json, math, os, pathlib, platform, re, shutil
import subprocess, sys, tarfile, tempfile, time, urllib.error, urllib.request

here = pathlib.Path(__file__).resolve().parent
root = here.parent.parent.parent
corpus = pathlib.Path(os.environ.get("RY_CORPUS_DIR", root / ".corpus")) / "packages"
mirror = "https://rrepo.dev/upstream/cran"
# One package's suite must not stall the whole table when an engine loops or deadlocks.
SUITE_TIMEOUT = 900
BASE_PACKAGES = {
    "R", "base", "compiler", "datasets", "graphics", "grDevices", "grid", "methods", "parallel",
    "splines", "stats", "stats4", "tcltk", "tools", "utils",
}

def main():
    parser = argparse.ArgumentParser()
    commands = parser.add_subparsers(dest="command", required=True)
    commands.add_parser("lock")
    install = commands.add_parser("install")
    install.add_argument("--jobs", type=int, default=os.cpu_count() or 1)
    run = commands.add_parser("run")
    run.add_argument("--ry", default=str(root / "target/release/ry"))
    run.add_argument("--filter", default="")
    run.add_argument("--repeat", type=int, default=1)
    run.add_argument("--write", action="store_true")
    args = parser.parse_args()
    {"lock": lock, "install": install_corpus, "run": run_corpus}[args.command](args)

# Resolution.

def roots():
    """The packages whose tests are timed and the extra packages their tests need, in the order
    packages.txt lists them: (name, [extras])."""
    entries = []
    for line in (here / "packages.txt").read_text().splitlines():
        line = line.split("#", 1)[0].strip()
        if line:
            name, *extras = line.split()
            entries.append((name, extras))
    return entries

def lock(args):
    r_version = gnu_r_version()
    wanted = []
    for name, extras in roots():
        wanted += [name, *extras]
    # Every suite needs its runner, whichever it uses.
    wanted += ["testthat", "tinytest"]
    resolved = {}
    queue = list(dict.fromkeys(wanted))
    while queue:
        name = queue.pop(0)
        if name in resolved or name in BASE_PACKAGES:
            continue
        version, description = newest_supported(name, r_version)
        resolved[name] = version
        for field in ("Depends", "Imports", "LinkingTo"):
            for dependency in dependencies(description.get(field, "")):
                if dependency not in resolved and dependency not in BASE_PACKAGES:
                    queue.append(dependency)
        print(f"{name} {version}", file=sys.stderr)
    with concurrent.futures.ThreadPoolExecutor(8) as pool:
        digests = dict(zip(resolved, pool.map(lambda item: sha256(fetch(source_url(*item))), resolved.items())))
    lines = ["# package\tversion\tsha256 of the source tarball; written by `run.py lock`"]
    lines += [f"{name}\t{resolved[name]}\t{digests[name]}" for name in sorted(resolved, key=str.lower)]
    (here / "lock.tsv").write_text("\n".join(lines) + "\n")
    print(f"locked {len(resolved)} packages for R {'.'.join(map(str, r_version))}", file=sys.stderr)

def newest_supported(name, r_version):
    """The newest version of `name` whose `R (>= ...)` requirement the installed GNU R meets, and
    its DESCRIPTION fields."""
    listing = json.loads(fetch(f"{mirror}/packages/{name}/versions"))
    versions = sorted((entry["version"] for entry in listing["versions"]), key=version_key, reverse=True)
    for version in versions:
        description = parse_dcf(fetch(f"{mirror}/packages/{name}/versions/{version}/description").decode())
        required = re.search(r"\bR\s*\(\s*>=?\s*([0-9.\-]+)\s*\)", description.get("Depends", ""))
        if required is None or version_key(required.group(1)) <= r_version:
            return version, description
    raise SystemExit(f"no version of {name} supports R {'.'.join(map(str, r_version))}")

def dependencies(field):
    names = []
    for item in field.split(","):
        name = re.sub(r"\(.*", "", item, flags=re.S).strip()
        if name:
            names.append(name)
    return names

def parse_dcf(text):
    fields, current = {}, None
    for line in text.splitlines():
        if line[:1].isspace() and current:
            fields[current] += " " + line.strip()
        elif ":" in line:
            current, value = line.split(":", 1)
            fields[current] = value.strip()
    return fields

def version_key(version):
    return tuple(int(part) for part in re.split(r"[.\-]", version) if part.isdigit())

def gnu_r_version():
    output = subprocess.run(["Rscript", "-e", "cat(as.character(getRversion()))"],
                            capture_output=True, text=True, check=True).stdout
    return version_key(output.strip())

# Installation.

def locked():
    entries = []
    for line in (here / "lock.tsv").read_text().splitlines():
        if line and not line.startswith("#"):
            name, version, digest = line.split("\t")
            entries.append((name, version, digest))
    return entries

def install_corpus(args):
    contrib = corpus / "repo" / "src" / "contrib"
    contrib.mkdir(parents=True, exist_ok=True)
    def download(entry):
        name, version, digest = entry
        target = contrib / f"{name}_{version}.tar.gz"
        if not (target.exists() and sha256(target.read_bytes()) == digest):
            data = fetch(source_url(name, version))
            if sha256(data) != digest:
                raise SystemExit(f"{name} {version}: the tarball does not match lock.tsv")
            target.write_bytes(data)
        return name
    with concurrent.futures.ThreadPoolExecutor(8) as pool:
        names = list(pool.map(download, locked()))
    library = corpus / "library"
    library.mkdir(parents=True, exist_ok=True)
    # The library holds only the locked versions, so a package left over from an older lock or
    # a newer one in a system library cannot shadow them.
    script = f"""
repo <- "file://{corpus / 'repo'}"
tools::write_PACKAGES("{contrib}", type = "source")
lib <- "{library}"
.libPaths(c(lib, .Library))
wanted <- c({', '.join(json.dumps(name) for name in names)})
have <- rownames(installed.packages(lib.loc = lib))
locked <- available.packages(repos = repo, type = "source")
current <- vapply(have, function(name) packageDescription(name, lib.loc = lib)$Version, "")
stale <- have[!(have %in% wanted) | current != locked[match(have, rownames(locked)), "Version"]]
if (length(stale)) remove.packages(stale, lib = lib)
missing <- setdiff(wanted, rownames(installed.packages(lib.loc = lib)))
if (length(missing)) install.packages(missing, lib = lib, repos = repo, type = "source",
                                      Ncpus = {args.jobs}, dependencies = FALSE)
failed <- setdiff(wanted, rownames(installed.packages(lib.loc = lib)))
if (length(failed)) message("not installed: ", paste(failed, collapse = ", "))
"""
    subprocess.run(["Rscript", "--vanilla", "-e", script], check=True,
                   env=dict(os.environ, MAKEFLAGS=f"-j{args.jobs}"))

# Running.

def run_corpus(args):
    library = corpus / "library"
    if not library.exists():
        raise SystemExit("no corpus library; run `run.py install` first")
    versions = {name: version for name, version, _ in locked()}
    gnu_r_paths = subprocess.run(["Rscript", "--vanilla", "-e", "cat(.Library)"],
                                 capture_output=True, text=True, check=True).stdout.strip()
    # Both engines see the corpus library first and then only base R's own, so neither reads a
    # different build of a package from a site library. NOT_CRAN stays unset, so suites skip
    # what they skip on CRAN, which keeps them short and free of network access.
    base = {key: value for key, value in os.environ.items() if key not in ("NOT_CRAN", "RY_JIT")}
    base.update(LC_ALL="C.UTF-8", R_LIBS=str(library), R_LIBS_USER=str(library), R_LIBS_SITE=gnu_r_paths,
                TESTTHAT_CPUS="1", _R_CHECK_FORCE_SUGGESTS_="false")
    engines = [
        ("gnu_r", "GNU R", ["Rscript", "--vanilla"]),
        ("ry", "ry", [args.ry, "run", "--runtime", "ry"]),
    ]
    rows = []
    for name, _ in roots():
        if args.filter and args.filter not in name:
            continue
        version = versions[name]
        work = pathlib.Path(tempfile.mkdtemp(prefix=f"ry-corpus-{name}-"))
        try:
            kind, tests = unpack_tests(name, version, work)
            row = {"package": name, "version": version, "runner": kind}
            results = {key: [] for key, _, _ in engines}
            for _ in range(args.repeat):
                for key, _, command in engines:
                    results[key].append(run_suite(command, base, name, kind, tests, work))
            for key, _, _ in engines:
                passed = [result for result in results[key] if result["seconds"] is not None]
                best = min(passed, key=lambda result: result["seconds"]) if passed else results[key][-1]
                row[key] = best
            rows.append(row)
            print(describe(row), file=sys.stderr)
        finally:
            shutil.rmtree(work, ignore_errors=True)
    print_table(rows, engines)
    if args.write:
        cpu, r_version = machine()
        (here / "results.json").write_text(json.dumps(
            {"machine": cpu, "r": r_version, "repeat": args.repeat, "packages": rows}, indent=2) + "\n")

def unpack_tests(name, version, work):
    """Extracts the package's tests from its locked tarball and says how they run."""
    tarball = corpus / "repo" / "src" / "contrib" / f"{name}_{version}.tar.gz"
    with tarfile.open(tarball) as archive:
        members = [member for member in archive.getmembers()
                   if member.name.startswith((f"{name}/tests/", f"{name}/inst/tinytest/"))
                   and ".." not in member.name and (member.isfile() or member.isdir())]
        archive.extractall(work, members=members, filter="data")
    package = work / name
    if (package / "tests" / "testthat").is_dir():
        return "testthat", package / "tests" / "testthat"
    if (package / "inst" / "tinytest").is_dir():
        return "tinytest", package / "inst" / "tinytest"
    if (package / "tests").is_dir():
        return "scripts", package / "tests"
    raise SystemExit(f"{name} {version} ships no tests")

def run_suite(command, env, name, kind, tests, work):
    """Wall seconds and the outcome of one run of a package's tests, or `None` seconds and why."""
    if kind == "scripts":
        return run_scripts(command, env, tests)
    if kind == "testthat":
        body = f"""
suppressPackageStartupMessages(library(testthat))
result <- as.data.frame(test_dir({json.dumps(str(tests))}, package = {json.dumps(name)},
  load_package = "installed", reporter = "silent", stop_on_failure = FALSE, stop_on_warning = FALSE))
cat("RESULT", sum(result$passed), sum(result$failed), sum(result$skipped), sum(result$error), "\\n")
"""
    else:
        body = f"""
suppressPackageStartupMessages(library({name}))
result <- as.data.frame(tinytest::run_test_dir({json.dumps(str(tests))}, at_home = FALSE, verbose = 0))
cat("RESULT", sum(result$result), sum(!result$result), 0L, 0L, "\\n")
"""
    driver = work / "driver.R"
    driver.write_text(body)
    started = time.monotonic()
    try:
        child = subprocess.run(command + [str(driver)], capture_output=True, text=True, env=env,
                               cwd=tests, timeout=SUITE_TIMEOUT)
    except subprocess.TimeoutExpired:
        return {"seconds": None, "outcome": f"timed out after {SUITE_TIMEOUT} s"}
    seconds = time.monotonic() - started
    lines = [line for line in child.stdout.splitlines() if line.startswith("RESULT ")]
    if child.returncode != 0 or not lines:
        detail = (child.stderr.strip() or child.stdout.strip() or "failed").splitlines()[-1]
        return {"seconds": None, "outcome": detail[:200]}
    passed, failed, skipped, errors = (int(value) for value in lines[-1].split()[1:])
    return {"seconds": round(seconds, 3), "passed": passed, "failed": failed, "skipped": skipped,
            "errors": errors, "outcome": f"{passed} passed, {failed} failed, {errors} errors"}

def run_scripts(command, env, tests):
    """Runs each tests/*.R script in the tests directory, as R CMD check does."""
    scripts = sorted(tests.glob("*.R"))
    started = time.monotonic()
    passed, failures = 0, []
    for script in scripts:
        try:
            child = subprocess.run(command + [script.name], capture_output=True, text=True, env=env,
                                   cwd=tests, timeout=SUITE_TIMEOUT)
        except subprocess.TimeoutExpired:
            return {"seconds": None, "outcome": f"{script.name} timed out after {SUITE_TIMEOUT} s"}
        if child.returncode == 0:
            passed += 1
        else:
            failures.append(script.name)
    seconds = time.monotonic() - started
    if failures:
        return {"seconds": None, "outcome": f"{len(failures)} of {len(scripts)} scripts failed: {', '.join(failures)}"[:200]}
    return {"seconds": round(seconds, 3), "passed": passed, "failed": 0, "skipped": 0, "errors": 0,
            "outcome": f"{passed} scripts passed"}

def describe(row):
    parts = [f"{row['package']} {row['version']} ({row['runner']})"]
    for key in ("gnu_r", "ry"):
        result = row[key]
        seconds = f"{result['seconds']:.2f} s" if result["seconds"] is not None else "fails"
        parts.append(f"{key}: {seconds}, {result['outcome']}")
    return " | ".join(parts)

def print_table(rows, engines):
    cpu, r_version = machine()
    print(f"Machine: {cpu}; {r_version}\n")
    print("| package | runner | GNU R | ry | GNU R / ry | GNU R outcome | ry outcome |")
    print("| --- | --- | ---: | ---: | ---: | --- | --- |")
    ratios = []
    for row in rows:
        gnu_r, ry = row["gnu_r"], row["ry"]
        cell = lambda result: f"{result['seconds']:.2f} s" if result["seconds"] is not None else "fails"
        ratio = ""
        if gnu_r["seconds"] is not None and ry["seconds"] is not None:
            ratios.append(gnu_r["seconds"] / ry["seconds"])
            ratio = f"{ratios[-1]:.2f}x"
        print(f"| {row['package']} {row['version']} | {row['runner']} | {cell(gnu_r)} | {cell(ry)} | {ratio} "
              f"| {gnu_r['outcome']} | {ry['outcome']} |")
    if ratios:
        geomean = math.exp(sum(map(math.log, ratios)) / len(ratios))
        print(f"\nGeometric mean of GNU R's time over ry's ({len(ratios)} of {len(rows)} packages run on both): {geomean:.2f}x")

# Helpers.

def source_url(name, version):
    return f"{mirror}/packages/{name}/versions/{version}/source"

def fetch(url):
    for attempt in range(4):
        try:
            # The mirror refuses urllib's default user agent.
            request = urllib.request.Request(url, headers={"User-Agent": "ry-bench-packages"})
            with urllib.request.urlopen(request, timeout=120) as response:
                return response.read()
        except (urllib.error.URLError, TimeoutError) as error:
            if isinstance(error, urllib.error.HTTPError) and error.code in (403, 404):
                raise SystemExit(f"{url}: {error}")
            if attempt == 3:
                raise
            time.sleep(2 ** (attempt + 1))

def sha256(data):
    return hashlib.sha256(data).hexdigest()

def machine():
    cpu = platform.processor() or platform.machine()
    try:
        for line in pathlib.Path("/proc/cpuinfo").read_text().splitlines():
            if line.startswith("model name"):
                cpu = line.split(":", 1)[1].strip()
                break
    except OSError:
        pass
    version = subprocess.run(["Rscript", "--version"], capture_output=True, text=True)
    return f"{cpu}, {os.cpu_count()} cores", (version.stdout + version.stderr).strip().splitlines()[0]

if __name__ == "__main__":
    main()
