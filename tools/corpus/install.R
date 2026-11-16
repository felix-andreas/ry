#!/usr/bin/env Rscript
# Installs the benchmark dependency closure, plus the Suggests of each root package,
# into a dedicated GNU R library. See docs/src/content/docs/development/corpus-and-oracle.md.
#
# Environment:
#   RY_CORPUS_DIR       output directory (default: <repo>/.corpus)
#   RY_CORPUS_SNAPSHOT  Posit Package Manager snapshot date (default: pinned below)
#   RY_CORPUS_NCPUS     parallel install jobs (default: 4)
#   RY_CORPUS_ROOTS     the package list to install (default: tools/corpus/roots.txt, the benchmark
#                       set, which also installs the GitHub remotes below); `just top-packages` passes
#                       tools/corpus/top-packages.txt

args <- commandArgs(trailingOnly = FALSE)
script <- sub("^--file=", "", grep("^--file=", args, value = TRUE))
repo_root <- normalizePath(file.path(dirname(script), "..", ".."))

corpus_dir <- Sys.getenv("RY_CORPUS_DIR", file.path(repo_root, ".corpus"))
snapshot <- Sys.getenv("RY_CORPUS_SNAPSHOT", "2025-03-01")
ncpus <- as.integer(Sys.getenv("RY_CORPUS_NCPUS", "4"))

# The benchmark dependency set from AGENTS.md, minus the GitHub remotes below, unless
# RY_CORPUS_ROOTS names another list.
roots_file <- Sys.getenv("RY_CORPUS_ROOTS", "")
roots <- readLines(if (nzchar(roots_file)) roots_file else file.path(repo_root, "tools", "corpus", "roots.txt"))
roots <- trimws(sub("#.*", "", roots))
roots <- roots[nzchar(roots)]
# Packages the benchmark takes from GitHub: name, URL, ref (NA means the default branch).
remotes <- if (nzchar(roots_file)) list() else list(
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

# GNU R skips a dependency its own `.Library` already holds, but ry's runtime reads only this library,
# so a non-priority package an R build ships there (the reference R ships testthat, rlang and about 50
# more) would be missing under ry. Copying those builds keeps this library self-contained.
bundled <- installed.packages(lib.loc = .Library, noCache = TRUE)
bundled <- setdiff(rownames(bundled)[is.na(bundled[, "Priority"])], installed())
if (length(bundled) > 0 && !all(file.copy(file.path(.Library, bundled), library_dir, recursive = TRUE))) {
  stop("cannot copy R's own packages into ", library_dir)
}

# Older R headers declared functions that libR exports but R 4.6's leave undeclared by default,
# such as `ATTRIB`, `ENCLOS` and `Rf_findVarInFrame`. With gcc 13, C code from the pinned snapshot
# that calls one compiles with only a warning, and the implicit `int` return truncates a returned
# pointer, so the package aborts at its first such call, under GNU R and ry alike (xts's `x[2:4]`,
# Matrix's `Cholesky`). A Posit binary and a package compiled here are equally affected, so the
# check reads each shared object's imports instead of its `Built:` field: a package that imports an
# exported function the default headers leave undeclared is reinstalled from the newest CRAN source,
# which compiles against those headers. A function whose prototype under `ENABLE_LEGACY_NONAPI`
# returns an `int` or nothing (`OBJECT`, `SET_ATTRIB`) survives the implicit declaration and does
# not count; one with no prototype at all (`ENCLOS`, `CLOENV`) does. A package that declares the
# function itself (S7 defines `ENABLE_LEGACY_NONAPI_FUNS`) is rebuilt for nothing and logged again
# below, and so is one whose newest source needs a newer dependency than the snapshot holds, which
# fails to install and leaves the old build in place.
undeclared_entry_points <- function() {
  libr <- file.path(R.home("lib"), "libR.so")
  if (!file.exists(libr) || !nzchar(Sys.which("nm"))) {
    message("libR.so or nm not found, so packages' imports are not checked")
    return(character())
  }
  include <- R.home("include")
  ext <- setdiff(list.files(file.path(include, "R_ext"), "\\.h$"), c("GraphicsEngine.h", "GraphicsDevice.h"))
  headers <- c(
    "R.h", "Rinternals.h", "Rmath.h", "Rdefines.h", "Rembedded.h", "Rinterface.h",
    "R_ext/GraphicsEngine.h", file.path("R_ext", ext)
  )
  source <- tempfile(fileext = ".h")
  writeLines(sprintf("#include <%s>", headers), source)
  r <- file.path(R.home("bin"), "R")
  preprocess <- function(compiler, defines = "") {
    command <- system2(r, c("CMD", "config", compiler), stdout = TRUE)
    language <- if (compiler == "CC") "c" else "c++"
    suppressWarnings(system(
      paste(command, "-E -P -w", defines, "-x", language, "-I", shQuote(include), shQuote(source), "2>/dev/null"),
      intern = TRUE
    ))
  }
  identifiers <- function(text) unlist(regmatches(text, gregexpr("[A-Za-z_][A-Za-z0-9_]*", text)))
  declared <- unique(c(identifiers(preprocess("CC")), identifiers(preprocess("CXX"))))
  legacy <- preprocess("CC", "-DENABLE_LEGACY_NONAPI")
  if (length(declared) == 0 || length(legacy) == 0) {
    message("the C preprocessor read nothing from R's headers, so packages' imports are not checked")
    return(character())
  }
  exported <- read.table(
    text = system2("nm", c("-D", "--defined-only", shQuote(libr)), stdout = TRUE),
    fill = TRUE, colClasses = "character", quote = "", comment.char = ""
  )
  # Fortran-callable helpers (`dblepr_` and the like) have no C prototype in any R version.
  candidates <- setdiff(exported[[3]][exported[[2]] %in% c("T", "W")], declared)
  candidates <- candidates[!grepl("_$", candidates)]
  statements <- trimws(strsplit(paste(legacy, collapse = " "), ";", fixed = TRUE)[[1]])
  returns_scalar <- vapply(candidates, function(name) {
    call <- sprintf("\\(?%s\\)?\\s*\\(", name)
    prototype <- grep(sprintf("^[^(]*[ *(]%s", call), statements, value = TRUE)
    length(prototype) > 0 && !grepl("\\*|\\bSEXP\\b", sub(sprintf("%s.*", call), "", prototype[1]))
  }, TRUE)
  candidates[!returns_scalar]
}

packages_calling_undeclared <- function(undeclared) {
  shared_objects <- Sys.glob(file.path(library_dir, "*", "libs", "*.so"))
  imports <- lapply(shared_objects, function(object) {
    sub("@.*", "", system2("nm", c("-D", "--undefined-only", "--format=just-symbols", shQuote(object)), stdout = TRUE))
  })
  hits <- lapply(imports, intersect, undeclared)
  package <- basename(dirname(dirname(shared_objects)))
  calls <- vapply(split(hits, package), function(found) paste(sort(unique(unlist(found))), collapse = " "), "")
  calls[nzchar(calls)]
}

undeclared <- undeclared_entry_points()
calls <- packages_calling_undeclared(undeclared)
if (length(calls) > 0) {
  cat(sprintf("imports a function R's headers do not declare: %s (%s)\n", names(calls), calls), sep = "")
  with_warnings(install.packages(
    names(calls), lib = library_dir, repos = c(CRAN = "https://cloud.r-project.org"),
    type = "source", dependencies = FALSE
  ))
  calls <- packages_calling_undeclared(undeclared)
  if (length(calls) > 0) {
    cat(sprintf("imports one after the rebuild (fine if it declares it itself): %s (%s)\n", names(calls), calls), sep = "")
  }
}

# A package newer than the snapshot (from the retry against the newest snapshot, or rebuilt above)
# can need a newer version of a dependency the snapshot already installed, and install.packages()
# never upgrades an installed dependency, so the package fails to load: Rdpack 2.6.6 imports
# rbibutils (> 2.4) while the snapshot holds 2.3. Each dependency that fails a version constraint is
# reinstalled from the newest CRAN source, and the check repeats because a newer dependency can
# need newer dependencies of its own.
unmet_constraints <- function() {
  fields <- c("Depends", "Imports", "LinkingTo")
  db <- installed.packages(lib.loc = c(library_dir, .Library), fields = fields, noCache = TRUE)
  db <- db[!duplicated(rownames(db)), , drop = FALSE]
  versions <- db[, "Version"]
  unmet <- list()
  for (package in rownames(db)) {
    entries <- trimws(unlist(strsplit(paste(db[package, fields][!is.na(db[package, fields])], collapse = ","), ",")))
    parts <- regmatches(entries, regexec("^([A-Za-z][A-Za-z0-9.]*)\\s*\\(\\s*([<>=]+)\\s*([^)]+?)\\s*\\)$", entries))
    for (part in parts[lengths(parts) == 4]) {
      dependency <- part[2]
      if (dependency == "R" || !dependency %in% names(versions)) next
      holds <- do.call(part[3], list(package_version(versions[[dependency]]), package_version(part[4])))
      if (!holds) unmet[[length(unmet) + 1]] <- sprintf("%s needs %s (%s %s), found %s", package, dependency, part[3], part[4], versions[[dependency]])
      if (!holds) names(unmet)[length(unmet)] <- dependency
    }
  }
  unmet
}
for (round in 1:3) {
  unmet <- unmet_constraints()
  if (length(unmet) == 0) break
  cat(sprintf("unmet version constraint: %s\n", unlist(unmet)), sep = "")
  with_warnings(install.packages(
    unique(names(unmet)), lib = library_dir, repos = c(CRAN = "https://cloud.r-project.org"), type = "source"
  ))
}
unmet <- unmet_constraints()
if (length(unmet) > 0) cat(sprintf("still unmet: %s\n", unlist(unmet)), sep = "")

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
    version = if (ok) as.character(packageVersion(package, lib.loc = c(library_dir, .Library))) else "",
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
