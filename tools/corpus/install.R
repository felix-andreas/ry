#!/usr/bin/env Rscript
# Installs the benchmark dependency closure, plus the Suggests of each root package,
# into a dedicated GNU R library. See docs/src/content/docs/contributing/corpus-and-oracle.md.
#
# Environment:
#   RY_CORPUS_DIR       output directory (default: <repo>/.corpus)
#   RY_CORPUS_SNAPSHOT  Posit Package Manager snapshot date (default: pinned below)
#   RY_CORPUS_NCPUS     parallel install jobs (default: 4)

args <- commandArgs(trailingOnly = FALSE)
script <- sub("^--file=", "", grep("^--file=", args, value = TRUE))
repo_root <- normalizePath(file.path(dirname(script), "..", ".."))

corpus_dir <- Sys.getenv("RY_CORPUS_DIR", file.path(repo_root, ".corpus"))
snapshot <- Sys.getenv("RY_CORPUS_SNAPSHOT", "2025-03-01")
ncpus <- as.integer(Sys.getenv("RY_CORPUS_NCPUS", "4"))

roots <- c(
  "abind", "assertthat", "azidentity", "AzureAuth", "AzureKeyVault", "AzureStor", "AzureTableStor",
  "brio", "cachem", "callr", "cli", "data.table", "DBI", "devtools", "digest", "doParallel",
  "FLSSS", "foreach", "fs", "git2r", "httr", "httr2", "jsonlite", "lgr", "lubridate", "memoise",
  "NlcOptim", "odbc", "ompr", "ompr.roi", "openxlsx", "partitions", "pdftools",
  "PerformanceAnalytics", "plumber", "polars", "pool", "progress", "purrr", "quadprog", "R6",
  "rappdirs", "Rblpapi", "RcppDE", "RcppSimdJson", "readr", "readxl", "renv", "Rglpk", "rlang",
  "rmgarch", "ROI.plugin.highs", "rugarch", "rvest", "snakecase", "stringr", "testthat",
  "tryCatchLog", "uuid", "withr", "xml2", "xts", "YieldCurve", "zeallot", "zoo"
)
# Packages the benchmark takes from GitHub: name, URL, ref (NA means the default branch).
remotes <- list(
  list(name = "esgtoolkit", url = "https://github.com/Techtonique/esgtoolkit", ref = "v1.0.0"),
  list(name = "loadfast", url = "https://github.com/finccam/loadfast", ref = NA)
)
extra_repo <- "https://ak-finccam.github.io/r-packages"

library_dir <- file.path(corpus_dir, "library")
log_dir <- file.path(corpus_dir, "logs")
source_dir <- file.path(corpus_dir, "src")
for (dir in c(library_dir, log_dir, source_dir)) dir.create(dir, recursive = TRUE, showWarnings = FALSE)

# The user agent makes Posit Package Manager answer with Linux binaries instead of sources.
options(HTTPUserAgent = sprintf(
  "R/%s R (%s)", getRversion(),
  paste(getRversion(), R.version$platform, R.version$arch, R.version$os)
))
distro <- if (file.exists("/etc/os-release")) sub("^VERSION_CODENAME=", "", grep("^VERSION_CODENAME=", readLines("/etc/os-release"), value = TRUE)) else character()
if (length(distro) != 1 || !nzchar(distro)) stop("cannot determine the Linux distribution from /etc/os-release")
options(repos = c(
  CRAN = sprintf("https://packagemanager.posit.co/cran/__linux__/%s/%s", distro, snapshot),
  FINCCAM = extra_repo
))
options(Ncpus = ncpus, timeout = 600, install.packages.compile.from.source = "always")
.libPaths(c(library_dir, .Library))
Sys.setenv(MAKEFLAGS = sprintf("-j%d", ncpus), R_LIBS_USER = library_dir)

log_file <- file.path(log_dir, "install.log")
log_connection <- file(log_file, open = "wt")
sink(log_connection, type = "output", split = TRUE)
sink(log_connection, type = "message")
warnings_seen <- character()
with_warnings <- function(expression) {
  withCallingHandlers(expression, warning = function(w) {
    warnings_seen <<- c(warnings_seen, conditionMessage(w))
    invokeRestart("muffleWarning")
  })
}

installed <- function() rownames(installed.packages(lib.loc = library_dir, noCache = TRUE))

with_warnings({
  install.packages("remotes", lib = library_dir)
  install.packages(roots, lib = library_dir, dependencies = TRUE)
})

# A package missing from the pinned snapshot gets one more try against the newest one.
missing_roots <- setdiff(roots, installed())
if (length(missing_roots) > 0) {
  with_warnings(install.packages(
    missing_roots, lib = library_dir, dependencies = TRUE,
    repos = c(CRAN = sprintf("https://packagemanager.posit.co/cran/__linux__/%s/latest", distro), FINCCAM = extra_repo)
  ))
}

for (remote in remotes) {
  checkout <- file.path(source_dir, remote$name)
  if (!dir.exists(checkout)) {
    status <- system2("git", c("clone", "--quiet", remote$url, checkout))
    if (status == 0 && !is.na(remote$ref)) status <- system2("git", c("-C", checkout, "checkout", "--quiet", remote$ref))
    if (status != 0) unlink(checkout, recursive = TRUE)
  }
  if (dir.exists(checkout)) {
    with_warnings({
      remotes::install_deps(checkout, lib = library_dir, dependencies = TRUE)
      install.packages(checkout, repos = NULL, type = "source", lib = library_dir)
    })
  }
}

# Record the outcome per package: every root and remote, with its reason when missing.
have <- rownames(installed.packages(noCache = TRUE))
targets <- c(roots, vapply(remotes, `[[`, "", "name"))
suggested <- unlist(lapply(intersect(roots, have), function(package) {
  tools::package_dependencies(package, db = installed.packages(lib.loc = library_dir), which = "Suggests")[[package]]
}))
suggested <- setdiff(unique(suggested), c(targets, rownames(installed.packages(priority = "base"))))
suggested_wanted <- sort(suggested)

reason_for <- function(package) {
  hits <- grep(sprintf("[‘'\"]%s[’'\"]", package), warnings_seen, value = TRUE, fixed = FALSE)
  if (length(hits) == 0) return("not installed; see logs/install.log")
  gsub("\\s+", " ", paste(unique(hits), collapse = " | "))
}
row <- function(package, role) {
  ok <- package %in% have
  data.frame(
    package = package, role = role, installed = ok,
    version = if (ok) as.character(packageVersion(package, lib.loc = library_dir)) else "",
    reason = if (ok) "" else reason_for(package)
  )
}
status <- do.call(rbind, c(
  lapply(targets, row, role = "closure"),
  lapply(suggested_wanted, row, role = "suggests")
))
status <- status[order(status$role, status$package), ]
write.csv(status, file.path(corpus_dir, "status.csv"), row.names = FALSE)

sink(type = "message")
sink(type = "output")
close(log_connection)
cat(sprintf(
  "closure: %d/%d installed; suggests: %d/%d installed\nlibrary: %s\nstatus: %s\n",
  sum(status$installed[status$role == "closure"]), sum(status$role == "closure"),
  sum(status$installed[status$role == "suggests"]), sum(status$role == "suggests"),
  library_dir, file.path(corpus_dir, "status.csv")
))
