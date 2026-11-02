#!/usr/bin/env python3
"""Generate the synthetic benchmark code bases: one R package each for finance, data
science and biostatistics.

Every package imports exactly the dependency set from AGENTS.md. Its R files come from the
templates in templates/ (shared infrastructure plus one directory per domain), instantiated
with seeded random parameters until the requested size is reached, and every instance ships
with a testthat test. The output is deterministic for a given seed.

Usage:
    python3 -I generate.py --out DIR [--domain NAME|all] [--lines N] [--files N] [--seed N]
                           [--typed] [--only TEMPLATE,...]

The templates carry `#:` type annotations. By default they are stripped, which gives the plain
R code base. `--typed` keeps them and turns on ry's strict mode in `ry.toml`, so ry checks the code
base as fully typed strict code against the dependency types shipped in `types/`.
`--only` restricts generation to the named templates, which is how a slice is checked alone.

Template format (see templates/README.md): header lines start with `#@`, the R code
follows, and a line `#@ test` starts the test code. Placeholders `@ID@` and `@<PARAM>@`
are replaced per instance.
"""

import argparse
import dataclasses
import os
import random
import re
import shutil
import sys

IMPORTS = """
abind assertthat azidentity>=1.1.0 AzureAuth AzureKeyVault AzureStor AzureTableStor
brio cachem callr cli data.table DBI devtools digest doParallel esgtoolkit>=1.0.0
FLSSS foreach fs git2r httr httr2 jsonlite lgr loadfast lubridate memoise methods
NlcOptim odbc ompr ompr.roi openxlsx parallel partitions pdftools PerformanceAnalytics
plumber polars pool progress purrr quadprog R6 rappdirs Rblpapi RcppDE RcppSimdJson
readr readxl renv Rglpk rlang rmgarch ROI.plugin.highs rugarch rvest snakecase stats
stringr testthat tools tryCatchLog utils uuid withr xml2 xts YieldCurve zeallot zoo
""".split()

DOMAINS = {
    "finance": ("finbench", "Synthetic finance code base for ry benchmarks: pricing, portfolio construction, risk and market-data adapters."),
    "datascience": ("dsbench", "Synthetic data science code base for ry benchmarks: table pipelines, features, models and serving."),
    "biostatistics": ("biobench", "Synthetic biostatistics code base for ry benchmarks: trials, survival, genetics and assay analysis."),
}

# A file is filled with instances until it reaches this many lines, so the file count
# follows from the line count unless --files overrides it.
DEFAULT_LINES_PER_FILE = 600
SHARED_SHARE = 0.25


@dataclasses.dataclass
class Template:
    name: str
    domain: str
    weight: int
    params: list
    ns: list
    code: str
    test: str


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--out", required=True, help="output directory; one package directory per domain")
    parser.add_argument("--domain", default="all", choices=["all", *DOMAINS])
    parser.add_argument("--lines", type=int, default=10_000, help="target R lines per package (excluding tests)")
    parser.add_argument("--files", type=int, help="number of R files per package (default: lines / 600)")
    parser.add_argument("--seed", type=int, default=1)
    parser.add_argument("--typed", action="store_true", help="keep the type annotations and turn on strict mode")
    parser.add_argument("--only", help="comma-separated template names to draw from, such as returns,caching")
    args = parser.parse_args()
    if args.lines < 1 or (args.files is not None and args.files < 1):
        parser.error("--lines and --files must be at least 1")

    here = os.path.dirname(os.path.abspath(__file__))
    templates = {domain: load_templates(os.path.join(here, "templates", domain), domain) for domain in [*DOMAINS, "shared"]}
    if args.only:
        wanted = set(args.only.split(","))
        known = {template.name for loaded in templates.values() for template in loaded}
        if wanted - known:
            parser.error(f"unknown templates: {', '.join(sorted(wanted - known))}")
        templates = {domain: [template for template in loaded if template.name in wanted] for domain, loaded in templates.items()}
    if not args.typed:
        templates = {domain: [strip_annotations(template) for template in loaded] for domain, loaded in templates.items()}
    for domain in DOMAINS if args.domain == "all" else [args.domain]:
        if not templates[domain] and not templates["shared"]:
            continue
        generate_package(args, domain, templates[domain] or templates["shared"], templates["shared"] or templates[domain])


def load_templates(directory, domain):
    loaded = []
    for name in sorted(os.listdir(directory)):
        if name.endswith(".R"):
            with open(os.path.join(directory, name), encoding="utf-8") as handle:
                loaded.append(parse_template(name[:-2], domain, handle.read()))
    return loaded


def parse_template(name, domain, text):
    weight, params, ns, code, test, in_test = 1, [], [], [], [], False
    for line in text.splitlines():
        if line.startswith("#@"):
            fields = line[2:].strip().split(None, 1)
            key, rest = fields[0], fields[1] if len(fields) > 1 else ""
            if key == "test":
                in_test = True
            elif key == "weight":
                weight = int(rest)
            elif key == "param":
                params.append(parse_param(rest))
            elif key == "ns":
                ns.append(rest)
            else:
                raise SystemExit(f"{domain}/{name}: unknown header `{key}`")
        else:
            (test if in_test else code).append(line)
    return Template(name, domain, weight, params, ns, "\n".join(code).strip("\n") + "\n", "\n".join(test).strip("\n") + "\n")


ANNOTATION = re.compile(r"^[ \t]*#:.*\n", flags=re.M)


def annotated(args, text):
    return text if args.typed else ANNOTATION.sub("", text)


def strip_annotations(template):
    return dataclasses.replace(template, code=ANNOTATION.sub("", template.code), test=ANNOTATION.sub("", template.test))


def parse_param(spec):
    # NAME int LOW HIGH | NAME float LOW HIGH DIGITS | NAME choice a|b|c
    name, kind, rest = spec.split(None, 2)
    if kind == "choice":
        return (name, "choice", rest.split("|"))
    return (name, kind, rest.split())


def draw(rng, param):
    name, kind, rest = param
    if kind == "int":
        return name, str(rng.randint(int(rest[0]), int(rest[1])))
    if kind == "float":
        return name, f"{rng.uniform(float(rest[0]), float(rest[1])):.{int(rest[2])}f}"
    return name, rng.choice(rest)


def generate_package(args, domain, domain_templates, shared_templates):
    package, title = DOMAINS[domain]
    rng = random.Random(f"{args.seed}:{domain}")
    root = os.path.join(args.out, package)
    shutil.rmtree(root, ignore_errors=True)
    for directory in ("R", "tests/testthat"):
        os.makedirs(os.path.join(root, directory))

    files = args.files or max(4, args.lines // DEFAULT_LINES_PER_FILE)
    lines_per_file = max(1, args.lines // files)
    namespace = set()
    exports = []
    counter = 0
    used_packages = set()

    for index in range(files):
        code_parts, test_parts, lines, label = [], [], 0, None
        while lines < lines_per_file:
            pool = shared_templates if rng.random() < SHARED_SHARE else domain_templates
            template = rng.choices(pool, [template.weight for template in pool])[0]
            counter += 1
            identifier = f"{counter:05d}"
            values = dict(draw(rng, param) for param in template.params)
            values["ID"] = identifier
            code = fill(template.code, values)
            code_parts.append(code)
            test_parts.append(fill(template.test, values))
            namespace.update(fill(line, values) for line in template.ns)
            exports.extend(exported_names(code))
            used_packages.update(re.findall(r"\b([A-Za-z][A-Za-z0-9.]*)::", code + template.test))
            # Counting the code without its annotations makes `--typed` generate the same program
            # as an untyped run with the same seed, so the two can be timed against each other.
            lines += ANNOTATION.sub("", code).count("\n") + 1
            label = label or template.name
        stem = f"{label.replace('_', '-')}-{index:04d}"
        write(os.path.join(root, "R", f"{stem}.R"), f"# {package}: {label}\n\n" + "\n".join(code_parts))
        write(os.path.join(root, "tests", "testthat", f"test-{stem}.R"), "\n".join(test_parts))

    write(os.path.join(root, "R", "zzz.R"), annotated(args, ZZZ.format(package=package)))
    write(os.path.join(root, "R", "dependencies.R"), annotated(args, dependencies_file()))
    write(os.path.join(root, "tests", "testthat", "test-dependencies.R"), annotated(args, DEPENDENCY_TEST))
    used_packages.update(re.findall(r"\b([A-Za-z][A-Za-z0-9.]*)::", dependencies_file()))
    exports.extend(["backend_registry", "dependency_versions", "missing_dependencies", "reload_package", "run_package_tests", "azure_token", "highs_available"])
    write(os.path.join(root, "DESCRIPTION"), description(package, title))
    write(os.path.join(root, "NAMESPACE"), namespace_file(exports, namespace))
    write(os.path.join(root, "tests", "testthat.R"), f"library(testthat)\nlibrary({package})\n\ntest_check(\"{package}\")\n")
    write(os.path.join(root, "tests", "testthat", "helper-seed.R"), annotated(args, HELPER))
    write(os.path.join(root, ".Rbuildignore"), "^dev$\n")
    write(os.path.join(root, "dev", "reload.R"), RELOAD.format(package=package))
    if args.typed:
        write(os.path.join(root, "ry.toml"), "[check]\nstrict = true\n")

    uncovered = sorted({name.split(">=")[0] for name in IMPORTS} - used_packages - {"zeallot", "ROI.plugin.highs"})
    total = sum(count_lines(os.path.join(root, "R", name)) for name in os.listdir(os.path.join(root, "R")))
    print(f"{package}: {files} files, {total} R lines, {counter} instances, {len(exports)} exports", file=sys.stderr)
    if uncovered:
        print(f"{package}: dependencies never referenced with `::`: {' '.join(uncovered)}", file=sys.stderr)


def fill(text, values):
    for key, value in values.items():
        text = text.replace(f"@{key}@", value)
    return text


def exported_names(code):
    return re.findall(r"^([A-Za-z][A-Za-z0-9._]*) <- (?:function|R6::R6Class)", code, flags=re.M)


def namespace_file(exports, extra):
    lines = ["# Generated by tools/bench/generate.py"]
    lines += [f"export({name})" for name in sorted(set(exports))]
    lines += sorted(extra)
    return "\n".join(lines) + "\n"


def description(package, title):
    imports = ",\n    ".join(IMPORTS_WITH_VERSIONS())
    return f"""Package: {package}
Title: {title.split(':')[0]}
Version: 0.1.0
Description: {title}
License: GPL-3
Encoding: UTF-8
Roxygen: list(markdown = TRUE)
Config/testthat/edition: 3
Imports:
    {imports}
Remotes:
    github::Techtonique/esgtoolkit@v1.0.0,
    github::finccam/loadfast
Additional_repositories:
    https://ak-finccam.github.io/r-packages
"""


def IMPORTS_WITH_VERSIONS():
    return [name.replace(">=", " (>= ") + ")" if ">=" in name else name for name in IMPORTS]


def count_lines(path):
    with open(path, encoding="utf-8") as handle:
        return sum(1 for _ in handle)


def write(path, text):
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "w", encoding="utf-8") as handle:
        handle.write(text)


# One exported entry point per dependency, so every package is referenced from every code base.
REGISTRY = {
    "abind": "abind", "assertthat": "assert_that", "azidentity": "default_azure_credential",
    "AzureAuth": "get_azure_token", "AzureKeyVault": "key_vault", "AzureStor": "storage_endpoint",
    "AzureTableStor": "table_endpoint", "brio": "read_lines", "cachem": "cache_mem", "callr": "r",
    "cli": "cli_text", "data.table": "data.table", "DBI": "dbConnect", "devtools": "load_all",
    "digest": "sha1", "doParallel": "registerDoParallel", "esgtoolkit": "simdiff",
    "FLSSS": "FLSSS", "foreach": "foreach", "fs": "path", "git2r": "init", "httr": "GET",
    "httr2": "request", "jsonlite": "fromJSON", "lgr": "get_logger", "loadfast": "load_fast",
    "lubridate": "make_date", "memoise": "memoise", "methods": "new", "NlcOptim": "solnl",
    "odbc": "odbc", "ompr": "MIPModel", "ompr.roi": "with_ROI", "openxlsx": "read.xlsx",
    "parallel": "mclapply", "partitions": "parts", "pdftools": "pdf_text",
    "PerformanceAnalytics": "Return.calculate", "plumber": "pr", "polars": "as_polars_df",
    "pool": "dbPool", "progress": "progress_bar$new", "purrr": "map", "quadprog": "solve.QP",
    "R6": "R6Class", "rappdirs": "user_data_dir", "Rblpapi": "blpConnect", "RcppDE": "DEoptim",
    "RcppSimdJson": "fparse", "readr": "read_csv", "readxl": "read_excel", "renv": "snapshot",
    "Rglpk": "Rglpk_solve_LP", "rlang": "abort", "rmgarch": "dccspec", "rugarch": "ugarchspec",
    "rvest": "read_html", "snakecase": "to_snake_case", "stats": "sd", "stringr": "str_detect",
    "testthat": "test_that", "tools": "file_ext", "tryCatchLog": "tryCatchLog", "utils": "str",
    "uuid": "UUIDgenerate", "withr": "with_dir", "xml2": "read_xml", "xts": "xts",
    "YieldCurve": "NSrates", "zoo": "zoo",
}


def dependencies_file():
    names = [name.split(">=")[0] for name in IMPORTS]
    packages = ",\n  ".join(f'"{name}"' for name in names)
    # The registry only names each class system to reference its package, so its lint is allowed.
    allow = {"R6": " # ry: allow(object-system)"}
    listed = [name for name in names if name in REGISTRY]
    entries = "\n    ".join(
        f"`{name}` = function(...) {name}::{REGISTRY[name]}(...){'' if index == len(listed) - 1 else ','}{allow.get(name, '')}"
        for index, name in enumerate(listed)
    )
    return DEPENDENCIES.replace("@PACKAGES@", packages).replace("@ENTRIES@", entries)


DEPENDENCIES = """# The packages this code base builds on, and the entry points that reload and test it.
#: character[]
backend_packages <- c(
  @PACKAGES@
)

#: fn() -> character[]
dependency_versions <- function() {
  vapply(backend_packages, function(name) as.character(utils::packageVersion(name)), character(1))
}

#: fn() -> character[]
missing_dependencies <- function() {
  installed <- vapply(backend_packages, requireNamespace, logical(1), quietly = TRUE)
  backend_packages[!installed]
}

# One thin wrapper per backend, resolved only when called.
#: fn() -> list[named: fn(...: Any) -> Any]
backend_registry <- function() {
  list(
    @ENTRIES@
  )
}

#: fn() -> logical
highs_available <- function() requireNamespace("ROI.plugin.highs", quietly = TRUE)

#: fn([scopes]: character[], [provider]: fn(scopes: character[]) -> character) -> character
azure_token <- function(scopes = "https://management.azure.com/.default", provider = azidentity::default_azure_credential) {
  provider(scopes)
}

#: fn([path]: character, [fast]: logical) -> environment | list[named: Any] | NULL
reload_package <- function(path = ".", fast = FALSE) {
  if (fast) loadfast::load_fast(path) else devtools::load_all(path)
}

#: fn([path]: character, [reporter]: character) -> Any
run_package_tests <- function(path = ".", reporter = "check") {
  testthat::test_local(path, reporter = reporter, stop_on_failure = FALSE)
}
"""

DEPENDENCY_TEST = """test_that("the registry names every backend", {
  registry <- backend_registry()
  expect_gt(length(registry), 60L)
  expect_true(all(vapply(registry, is.function, logical(1))))
  expect_true(all(c("data.table", "R6", "rlang") %in% names(registry)))
  expect_type(missing_dependencies(), "character")
  expect_type(highs_available(), "logical")
})
"""

ZZZ = """.datatable.aware <- TRUE

#: fn(libname: character, pkgname: character) -> NULL
.onLoad <- function(libname, pkgname) {{
  options(
    {package}.verbose = getOption("{package}.verbose", FALSE),
    {package}.seed = getOption("{package}.seed", 20240229L)
  )
  invisible()
}}
"""

HELPER = """set.seed(20240229)
options(cli.num_colors = 1, cli.unicode = FALSE, lgr.console = FALSE)
"""

RELOAD = """# Development helper: reload the package from source.
devtools::load_all("{package}")
"""

if __name__ == "__main__":
    main()
