#!/usr/bin/env python3
"""Measure which parts of R's C API the benchmark dependency closure uses.

The script resolves the dependency closure of the benchmark package set
(Depends, Imports and LinkingTo, recursively) against CRAN, the extra
repository and the two GitHub remotes, downloads every source tarball, and
scans each package's compiled code for identifiers declared in R's public
headers. It writes three CSV files:

- symbols.csv: one row per API symbol, with how many packages use it, how
  often it occurs, and how R classifies it (documented API, experimental API,
  embedding API, explicitly non-API, or declared in a public header without a
  classification).
- packages.csv: one row per package, with its languages, its binding layer
  (Rcpp, cpp11, savvy, extendr, plain C), and markers for the interfaces that
  shape a runtime (ALTREP, cross-package C callables, external pointers,
  unwind protection, non-API entry points).
- r_calls.csv: per package, how often its R code calls .Call, .External, .C,
  .Fortran and .Internal.

Usage:
    python3 -I r_api_usage.py --work DIR --out DIR

DIR for --work receives downloads and extracted sources; it must not contain
the script. Only the standard library is used. Downloaded files are treated as
data: archives are extracted with tarfile's "data" filter and nothing inside
them is executed.
"""

import argparse
import collections
import concurrent.futures
import csv
import io
import os
import re
import shutil
import subprocess
import sys
import tarfile
import urllib.request

CRAN = "https://cloud.r-project.org/src/contrib"
EXTRA = "https://ak-finccam.github.io/r-packages/src/contrib"
R_SOURCE = "https://cloud.r-project.org/src/base/R-4/R-4.6.2.tar.gz"
# GitHub remotes are cloned rather than downloaded as archives, because the
# sandbox this was written in allows git but not GitHub's archive endpoints.
GITHUB_REMOTES = {
    "esgtoolkit": ("https://github.com/Techtonique/esgtoolkit", "v1.0.0"),
    "loadfast": ("https://github.com/finccam/loadfast", None),
}

ROOTS = """
abind assertthat azidentity AzureAuth AzureKeyVault AzureStor AzureTableStor
brio cachem callr cli data.table DBI devtools digest doParallel esgtoolkit
FLSSS foreach fs git2r httr httr2 jsonlite lgr loadfast lubridate memoise
methods NlcOptim odbc ompr ompr.roi openxlsx parallel partitions pdftools
PerformanceAnalytics plumber polars pool progress purrr quadprog R6 rappdirs
Rblpapi RcppDE RcppSimdJson readr readxl renv Rglpk rlang rmgarch
ROI.plugin.highs rugarch rvest snakecase stats stringr testthat tools
tryCatchLog utils uuid withr xml2 xts YieldCurve zeallot zoo
""".split()

BASE_PACKAGES = set(
    "R base compiler datasets graphics grDevices grid methods parallel splines "
    "stats stats4 tcltk tools utils".split()
)

PUBLIC_HEADERS = [
    "R.h", "Rinternals.h", "Rdefines.h", "Rembedded.h", "Rinterface.h",
    "Rconnections.h", "Rmath.h0.in",
]

C_KEYWORDS = set(
    """auto break case char const continue default do double else enum extern
    float for goto if inline int long register restrict return short signed
    sizeof static struct switch typedef union unsigned void volatile while
    defined bool true false NULL size_t FILE va_list wchar_t
    attribute_hidden LibExtern LibImport LibExport R_INLINE elif uintptr_t
    isfinite isnan""".split()
)

# Names the declaration patterns pick up from macro parameters and from
# function-pointer members of the graphics device structs; in package code
# they are almost always the package's own identifiers.
GENERIC_NAMES = set("name format fun fd port cell buff ptr size col visibility ctxt dtype rs_ ch_ cg_ rg_".split())

SOURCE_EXTENSIONS = {
    ".c": "C", ".h": "C/C++ header", ".cc": "C++", ".cpp": "C++", ".cxx": "C++",
    ".hpp": "C/C++ header", ".hh": "C/C++ header", ".f": "Fortran",
    ".f90": "Fortran", ".f95": "Fortran", ".rs": "Rust",
}

R_HEADER_INCLUDE = re.compile(
    r'#\s*include\s*[<"]\s*(R\.h|Rinternals\.h|Rdefines\.h|Rmath\.h|S\.h|'
    r'R_ext/[^>"]+|Rcpp[^>"]*|cpp11[^>"]*|RcppArmadillo[^>"]*|RcppEigen[^>"]*|'
    r'Rconfig\.h|Rversion\.h|Rembedded\.h|Rinterface\.h)\s*[>"]'
)
LOCAL_INCLUDE = re.compile(r'#\s*include\s*"([^"]+)"')
IDENTIFIER = re.compile(r"\b[A-Za-z_][A-Za-z0-9_]*\b")
R_CALL = re.compile(r"(?<![A-Za-z0-9_.])\.(Call|External2?|C|Fortran|Internal|Primitive)\s*\(")


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--work", required=True)
    parser.add_argument("--out", required=True)
    args = parser.parse_args()
    os.makedirs(args.work, exist_ok=True)
    os.makedirs(args.out, exist_ok=True)

    api = load_api(args.work)
    cran = parse_packages(fetch(f"{CRAN}/PACKAGES").decode("utf-8", "replace"))
    extra = parse_packages(fetch(f"{EXTRA}/PACKAGES").decode("utf-8", "replace"))
    remotes = {name: clone_remote(args.work, name, *remote) for name, remote in GITHUB_REMOTES.items()}

    closure, sources = resolve_closure(cran, extra, remotes)
    print(f"closure: {len(closure)} packages", file=sys.stderr)

    with concurrent.futures.ThreadPoolExecutor(max_workers=12) as pool:
        roots = dict(zip(
            sorted(closure),
            pool.map(lambda name: extract_package(args.work, name, sources[name], remotes), sorted(closure)),
        ))

    package_rows = []
    symbol_packages = collections.defaultdict(set)
    symbol_occurrences = collections.Counter()
    call_rows = []
    for name in sorted(closure):
        root = roots[name]
        if root is None:
            print(f"missing source: {name}", file=sys.stderr)
            continue
        description = closure[name]
        usage, languages = scan_compiled(root, api)
        for symbol, count in usage.items():
            symbol_packages[symbol].add(name)
            symbol_occurrences[symbol] += count
        package_rows.append(package_row(name, description, usage, languages, api))
        call_rows.append({"package": name, **scan_r_calls(root)})

    write_csv(os.path.join(args.out, "packages.csv"), package_rows)
    write_csv(os.path.join(args.out, "r_calls.csv"), call_rows)
    symbol_rows = [
        {
            "symbol": symbol,
            "class": api["class"].get(symbol, "header"),
            "packages": len(symbol_packages[symbol]),
            "occurrences": symbol_occurrences[symbol],
            "used_by": " ".join(sorted(symbol_packages[symbol])),
        }
        for symbol in sorted(symbol_packages, key=lambda s: (-len(symbol_packages[s]), -symbol_occurrences[s], s))
    ]
    write_csv(os.path.join(args.out, "symbols.csv"), symbol_rows)


def load_api(work):
    """Read the public API universe and R's own classification of it."""
    archive = os.path.join(work, "R-4.6.2.tar.gz")
    if not os.path.exists(archive):
        with open(archive, "wb") as file:
            file.write(fetch(R_SOURCE))
    headers = {}
    wre = ""
    sotools = ""
    with tarfile.open(archive) as tar:
        for member in tar:
            path = member.name.split("/", 1)[-1]
            if path.startswith("src/include/"):
                relative = path[len("src/include/"):]
                if relative in PUBLIC_HEADERS or (relative.startswith("R_ext/") and relative.endswith(".h")):
                    headers[relative] = read_member(tar, member)
            elif path == "doc/manual/R-exts.texi":
                wre = read_member(tar, member)
            elif path == "src/library/tools/R/sotools.R":
                sotools = read_member(tar, member)

    aliases = {}
    declared = set()
    for text in headers.values():
        text = re.sub(r"/\*.*?\*/", " ", text, flags=re.S)
        text = re.sub(r"//[^\n]*", " ", text)
        for match in re.finditer(r"^\s*#\s*define\s+([A-Za-z_]\w*)(?:\((\w+)\))?[ \t]+([A-Za-z_]\w*)(?:\(\2\))?[ \t]*$", text, re.M):
            alias, _, target = match.groups()
            if target.startswith("Rf_") or target.startswith("R_") or alias.islower():
                aliases[alias] = target
        for match in re.finditer(r"^\s*#\s*define\s+([A-Za-z_]\w*)", text, re.M):
            declared.add(match.group(1))
        pointer_members = set(re.findall(r"\(\s*\*\s*([A-Za-z_]\w*)\s*\)", text))
        for match in re.finditer(r"\b([A-Za-z_]\w*)\s*\(", text):
            if match.group(1) not in pointer_members:
                declared.add(match.group(1))
        # Rinternals.h parenthesizes names that are also macros: `R_xlen_t (TRUELENGTH)(SEXP x);`.
        for match in re.finditer(r"\(\s*([A-Za-z_]\w*)\s*\)\s*\(", text):
            declared.add(match.group(1))
        for match in re.finditer(r"\bLibExtern\s+[\w\s\*]*?\b([A-Za-z_]\w*)\s*(?:\[[^\]]*\])?\s*;", text):
            declared.add(match.group(1))
        for match in re.finditer(r"\benum\b[^{;]*\{([^}]*)\}", text, re.S):
            for constant in re.finditer(r"\b([A-Za-z_]\w*)\s*(?:=[^,]*)?(?:,|$)", match.group(1)):
                declared.add(constant.group(1))
        for match in re.finditer(r"\btypedef\b[^;]*?\b([A-Za-z_]\w*)\s*;", text, re.S):
            declared.add(match.group(1))
    declared -= C_KEYWORDS | GENERIC_NAMES
    declared = {name for name in declared if len(name) > 1 and not name.startswith("__") and not name.endswith("_H_")}

    classes = {}
    for kind, label in (("api", "api"), ("eapi", "experimental"), ("emb", "embedding")):
        for match in re.finditer(rf"^@{kind}(?:fun|var)\s+(\S+)", wre, re.M):
            classes.setdefault(canonical_wre(match.group(1)), label)
    for name in r_string_vector(sotools, "nonAPI <- c("):
        classes.setdefault(name, "non-api")

    def canonical(name):
        return aliases.get(name, name)

    universe = {canonical(name) for name in declared} | set(aliases.values())
    # WRE lists names without the Rf_ prefix; map them onto what the header declares.
    resolved = {}
    for name, label in classes.items():
        if name in GENERIC_NAMES:
            continue
        for candidate in (name, f"Rf_{name}", aliases.get(name, "")):
            if candidate in universe:
                resolved.setdefault(candidate, label)
        if name not in universe and f"Rf_{name}" not in universe:
            resolved.setdefault(name, label)
            universe.add(name)
    return {
        "aliases": aliases,
        "universe": universe,
        "class": resolved,
        "remappable": {alias for alias, target in aliases.items() if target.startswith("Rf_")},
    }


def r_string_vector(source, opener):
    """Return the string literals of the R call that starts at `opener`, skipping comments."""
    start = source.find(opener)
    if start < 0:
        return []
    names = []
    depth = 0
    position = start + len(opener) - 1
    while position < len(source):
        character = source[position]
        if character == "#":
            position = source.find("\n", position)
            if position < 0:
                break
        elif character == '"':
            end = source.find('"', position + 1)
            names.append(source[position + 1:end])
            position = end
        elif character == "(":
            depth += 1
        elif character == ")":
            depth -= 1
            if depth == 0:
                break
        position += 1
    return names


def canonical_wre(name):
    return name.strip().rstrip(",")


def resolve_closure(cran, extra, remotes):
    """Walk Depends, Imports and LinkingTo from the roots; the extra repository wins over CRAN."""
    index = {}
    sources = {}
    for name, entries in cran.items():
        index[name] = entries[-1]
        sources[name] = f"{CRAN}/{name}_{entries[-1]['Version']}.tar.gz"
    for name, entries in extra.items():
        index[name] = entries[-1]
        sources[name] = f"{EXTRA}/{entries[-1]['File']}"
    for name, description in remotes.items():
        index[name] = description
        sources[name] = os.path.join("git", name)

    closure = {}
    queue = [name for name in ROOTS if name not in BASE_PACKAGES]
    while queue:
        name = queue.pop()
        if name in closure or name in BASE_PACKAGES:
            continue
        if name not in index:
            print(f"unresolved dependency: {name}", file=sys.stderr)
            continue
        closure[name] = index[name]
        for field in ("Depends", "Imports", "LinkingTo"):
            queue.extend(dependency_names(index[name].get(field, "")))
    return closure, sources


def dependency_names(field):
    names = []
    for part in field.split(","):
        part = re.sub(r"\(.*?\)", "", part).strip()
        if part:
            names.append(part)
    return names


def parse_packages(text):
    packages = collections.defaultdict(list)
    for block in text.strip().split("\n\n"):
        record = parse_dcf(block)
        if "Package" in record:
            packages[record["Package"]].append(record)
    return packages


def parse_dcf(text):
    record = {}
    key = None
    for line in text.splitlines():
        if line[:1].isspace() and key:
            record[key] += " " + line.strip()
        elif ":" in line:
            key, value = line.split(":", 1)
            key = key.strip()
            record[key] = value.strip()
    return record


def fetch(url):
    request = urllib.request.Request(url, headers={"User-Agent": "ry-research"})
    with urllib.request.urlopen(request, timeout=300) as response:
        return response.read()


def clone_remote(work, name, url, ref):
    checkout = os.path.join(work, "git", name)
    if not os.path.isdir(checkout):
        command = ["git", "clone", "--quiet", "--depth", "1"]
        if ref:
            command += ["--branch", ref]
        subprocess.run(command + [url, checkout], check=True)
    with open(os.path.join(checkout, "DESCRIPTION"), encoding="utf-8", errors="replace") as file:
        return parse_dcf(file.read())


def extract_package(work, name, url, remotes):
    target = os.path.join(work, "src", name)
    if os.path.isdir(target):
        return target
    if name in remotes:
        checkout = os.path.join(work, url)
        os.makedirs(target)
        for entry in ("DESCRIPTION", "NAMESPACE", "src", "R", "inst/include"):
            source = os.path.join(checkout, entry)
            if os.path.isdir(source):
                shutil.copytree(source, os.path.join(target, entry), symlinks=True)
            elif os.path.isfile(source):
                shutil.copy(source, os.path.join(target, entry))
        return target
    archive = os.path.join(work, "archives", f"{name}.tar.gz")
    if not os.path.exists(archive):
        try:
            data = fetch(url)
        except Exception as error:  # noqa: BLE001 - try the archive location before giving up
            try:
                version = re.search(r"_([^_/]+)\.tar\.gz$", url).group(1)
                data = fetch(f"{CRAN}/Archive/{name}/{name}_{version}.tar.gz")
            except Exception:  # noqa: BLE001
                print(f"download failed: {name}: {error}", file=sys.stderr)
                return None
        with open(archive, "wb") as file:
            file.write(data)
    staging = target + ".partial"
    os.makedirs(staging, exist_ok=True)
    with tarfile.open(archive) as tar:
        members = []
        for member in tar:
            parts = member.name.split("/", 1)
            if len(parts) < 2 or not (member.isfile() or member.isdir()):
                continue
            rest = parts[1]
            if rest in ("DESCRIPTION", "NAMESPACE") or rest.startswith(("src/", "R/", "inst/include/")):
                member.name = rest
                members.append(member)
        tar.extractall(staging, members=members, filter="data")
    os.rename(staging, target)
    return target


def scan_compiled(root, api):
    """Count API identifiers in compiled sources that can see R's headers."""
    files = {}
    for directory in ("src", "inst/include"):
        base = os.path.join(root, directory)
        for current, _, names in os.walk(base):
            for file_name in names:
                extension = os.path.splitext(file_name)[1].lower()
                if extension in SOURCE_EXTENSIONS:
                    path = os.path.join(current, file_name)
                    try:
                        with open(path, encoding="utf-8", errors="replace") as file:
                            files[path] = file.read()
                    except OSError as error:
                        print(f"unreadable: {path}: {error}", file=sys.stderr)

    languages = collections.Counter()
    for path, text in files.items():
        languages[SOURCE_EXTENSIONS[os.path.splitext(path)[1].lower()]] += text.count("\n")

    by_basename = collections.defaultdict(list)
    for path in files:
        by_basename[os.path.basename(path)].append(path)
    r_facing = {path for path, text in files.items() if R_HEADER_INCLUDE.search(text)}
    changed = True
    while changed:
        changed = False
        for path, text in files.items():
            if path in r_facing or path.endswith(".rs"):
                continue
            for include in LOCAL_INCLUDE.findall(text):
                if any(candidate in r_facing for candidate in by_basename.get(os.path.basename(include), [])):
                    r_facing.add(path)
                    changed = True
                    break

    usage = collections.Counter()
    aliases = api["aliases"]
    universe = api["universe"]
    remappable = api["remappable"]
    for path, text in files.items():
        is_rust = path.endswith(".rs")
        if not is_rust and path not in r_facing:
            continue
        if path.lower().endswith((".f", ".f90", ".f95")):
            continue
        text = strip_comments(text, is_rust)
        no_remap = is_rust or "R_NO_REMAP" in text or re.search(r"#\s*include\s*[<\"](Rcpp|cpp11)", text)
        for token in IDENTIFIER.findall(text):
            if token in remappable:
                if no_remap:
                    continue
                token = aliases[token]
            elif token in aliases:
                token = aliases[token]
            if token in universe:
                usage[token] += 1
    return usage, languages


def strip_comments(text, is_rust):
    text = re.sub(r"/\*.*?\*/", " ", text, flags=re.S)
    text = re.sub(r"//[^\n]*", " ", text)
    text = re.sub(r'"(?:\\.|[^"\\\n])*"', '""', text)
    return text


def package_row(name, description, usage, languages, api):
    linking = " ".join(dependency_names(description.get("LinkingTo", "")))
    layer = []
    for framework in ("Rcpp", "cpp11", "RcppArmadillo", "RcppEigen", "BH"):
        if re.search(rf"(^| ){re.escape(framework)}( |$)", linking):
            layer.append(framework)
    if languages.get("Rust"):
        layer.append("Rust")
    classes = api["class"]
    def any_of(prefixes):
        return sum(count for symbol, count in usage.items() if symbol.startswith(prefixes))
    return {
        "package": name,
        "version": description.get("Version", ""),
        "needs_compilation": description.get("NeedsCompilation", ""),
        "linking_to": linking,
        "binding_layer": " ".join(layer) or ("C" if usage else ""),
        "lines_c": languages.get("C", 0),
        "lines_cpp": languages.get("C++", 0),
        "lines_headers": languages.get("C/C++ header", 0),
        "lines_fortran": languages.get("Fortran", 0),
        "lines_rust": languages.get("Rust", 0),
        "distinct_symbols": len(usage),
        "occurrences": sum(usage.values()),
        "non_api_symbols": " ".join(sorted(s for s in usage if classes.get(s) == "non-api")),
        "altrep": any_of(("R_make_alt", "R_set_alt", "ALTREP", "R_altrep", "ALTVEC", "ALTINTEGER", "ALTREAL", "ALTSTRING", "ALTLOGICAL")),
        "ccallable": any_of(("R_RegisterCCallable", "R_GetCCallable")),
        "external_ptr": any_of(("R_MakeExternalPtr", "R_ExternalPtr", "R_ClearExternalPtr", "R_RegisterCFinalizer", "R_MakeWeakRef")),
        "unwind": any_of(("R_UnwindProtect", "R_ContinueUnwind", "R_MakeUnwindCont", "R_ToplevelExec", "R_tryEval", "R_tryCatch")),
        "registration": any_of(("R_registerRoutines", "R_useDynamicSymbols", "R_forceSymbols")),
        "data_pointers": any_of(("DATAPTR", "STRING_PTR", "INTEGER", "REAL", "LOGICAL", "COMPLEX", "RAW")),
    }


def scan_r_calls(root):
    counts = collections.Counter()
    base = os.path.join(root, "R")
    for current, _, names in os.walk(base):
        for file_name in names:
            if not file_name.lower().endswith((".r", ".s", ".q")):
                continue
            with open(os.path.join(current, file_name), encoding="utf-8", errors="replace") as file:
                for line in file:
                    code = line.split("#", 1)[0]
                    for match in R_CALL.finditer(code):
                        counts[match.group(1)] += 1
    return {kind: counts.get(kind, 0) for kind in ("Call", "External", "External2", "C", "Fortran", "Internal", "Primitive")}


def read_member(tar, member):
    file = tar.extractfile(member)
    return file.read().decode("utf-8", "replace") if file else ""


def write_csv(path, rows):
    if not rows:
        return
    with open(path, "w", newline="") as file:
        writer = csv.DictWriter(file, fieldnames=list(rows[0].keys()))
        writer.writeheader()
        writer.writerows(rows)


if __name__ == "__main__":
    main()
