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

`tools/bench/bench.py packages` times the suites with the helpers below.

A package's tests run the way R CMD check runs them: testthat suites through `test_dir()` on the
installed package, tinytest suites through `run_test_dir()`, and other packages' tests/*.R scripts
one by one. The time is the wall time of the whole process, so it includes start-up and loading
the package and its imports, as real use does.

The corpus runs offline, because a network round trip measures the network, not the engine, and
costs both engines the same. Test files that need the network are listed in NETWORK_TESTS and left
out. Both engines send every HTTP and HTTPS request through a local proxy that refuses it and
records the host, and a run in which any test reached for the network fails once the table is
printed, naming the package and the host, so a new network test cannot slip in silently.

Usage: tools/bench/tooling/packages.py lock | install [--jobs N]
"""
import argparse, concurrent.futures, hashlib, json, os, pathlib, re
import socketserver, subprocess, sys, tarfile, threading, time, urllib.error, urllib.parse
import urllib.request

here = pathlib.Path(__file__).resolve().parent.parent / "packages"
root = here.parent.parent.parent
corpus = pathlib.Path(os.environ.get("RY_CORPUS_DIR", root / ".corpus")) / "packages"
mirror = "https://rrepo.dev/upstream/cran"
# One package's suite must not stall the whole table when an engine loops or deadlocks.
SUITE_TIMEOUT = 900
# Test files left out of the corpus because they need the network, by package, each with a glob
# relative to the package's tests directory and the reason.
NETWORK_TESTS = {
    "jsonlite": [
        ("testthat/test-network-*.R",
         "fromJSON() on api.github.com URLs; it has no skip_on_cran(), so it runs offline too"),
    ],
}
# Hosts that tests may reach through the proxy's bypass: a package's own local servers.
LOCAL_HOSTS = "localhost,127.0.0.1,::1"
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
    args = parser.parse_args()
    {"lock": lock, "install": install_corpus}[args.command](args)

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
    lines = ["# package\tversion\tsha256 of the source tarball; written by `just bench-packages lock`"]
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

def source_tarball(name, version, digest):
    """The package's locked source tarball in the corpus repository, downloaded when missing."""
    target = corpus / "repo" / "src" / "contrib" / f"{name}_{version}.tar.gz"
    if not (target.exists() and sha256(target.read_bytes()) == digest):
        data = fetch(source_url(name, version))
        if sha256(data) != digest:
            raise SystemExit(f"{name} {version}: the tarball does not match lock.tsv")
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_bytes(data)
    return target

def install_corpus(args):
    # The binary cache keys its copy on lock.tsv and the reference R, so a restored library is
    # exactly the locked set, built against the R that runs it (tools/cache.sh).
    library = corpus / "library"
    restored = not library.is_dir() and subprocess.run(["bash", str(root / "tools/cache.sh"), "restore", "corpus"]).returncode == 0
    contrib = corpus / "repo" / "src" / "contrib"
    with concurrent.futures.ThreadPoolExecutor(8) as pool:
        names = [name for name, _, _ in locked()]
        list(pool.map(lambda entry: source_tarball(*entry), locked()))
    # The cache holds the installed library only; the runner unpacks each package's tests from
    # the source tarballs downloaded above.
    if restored:
        return
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

def unpack_tests(name, version, work):
    """Extracts the package's tests from its locked tarball and says how they run, or `None` when
    it ships none."""
    # A library restored from the binary cache comes without the source tarballs.
    digest = next(digest for locked_name, locked_version, digest in locked() if (locked_name, locked_version) == (name, version))
    with tarfile.open(source_tarball(name, version, digest)) as archive:
        members = [member for member in archive.getmembers()
                   if member.name.startswith((f"{name}/tests/", f"{name}/inst/tinytest/"))
                   and ".." not in member.name and (member.isfile() or member.isdir())]
        archive.extractall(work, members=members, filter="data")
    package = work / name
    for pattern, _ in NETWORK_TESTS.get(name, []):
        for test in (package / "tests").glob(pattern):
            test.unlink()
    if (package / "tests" / "testthat").is_dir():
        return "testthat", package / "tests" / "testthat"
    if (package / "inst" / "tinytest").is_dir():
        return "tinytest", package / "inst" / "tinytest"
    if (package / "tests").is_dir():
        return "scripts", package / "tests"
    return None

def driver(name, kind, tests):
    """The R script that runs a testthat or tinytest suite on the installed package and prints
    `RESULT <passed> <failed> <skipped> <errors>`."""
    if kind == "testthat":
        return f"""
suppressPackageStartupMessages(library(testthat))
result <- as.data.frame(test_dir({json.dumps(str(tests))}, package = {json.dumps(name)},
  load_package = "installed", reporter = "silent", stop_on_failure = FALSE, stop_on_warning = FALSE))
cat("\\nRESULT", sum(result$passed), sum(result$failed), sum(result$skipped), sum(result$error), "\\n")
"""
    return f"""
suppressPackageStartupMessages(library({name}))
result <- as.data.frame(tinytest::run_test_dir({json.dumps(str(tests))}, at_home = FALSE, verbose = 0))
cat("\\nRESULT", sum(result$result), sum(!result$result), 0L, 0L, "\\n")
"""

class RefusingProxy:
    """An HTTP proxy on a local port that answers every request with 403 and records its host,
    so an offline run fails fast instead of waiting on the network, and says what it tried."""

    def __init__(self):
        self.hosts = []
        self.lock = threading.Lock()
        proxy = self

        class Handler(socketserver.StreamRequestHandler):
            def handle(self):
                line = self.rfile.readline(65536).decode("latin-1").split()
                if len(line) >= 2:
                    target = line[1]
                    host = target if line[0] == "CONNECT" else urllib.parse.urlsplit(target).netloc
                    with proxy.lock:
                        proxy.hosts.append(host)
                self.wfile.write(b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")

        server = socketserver.ThreadingTCPServer(("127.0.0.1", 0), Handler)
        server.daemon_threads = True
        threading.Thread(target=server.serve_forever, daemon=True).start()
        self.url = f"http://127.0.0.1:{server.server_address[1]}"

    def take(self):
        with self.lock:
            hosts, self.hosts = self.hosts, []
        return hosts

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

if __name__ == "__main__":
    main()
