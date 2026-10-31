#!/usr/bin/env Rscript
# Downloads the source tarball of every package in the corpus closure, at the version installed in
# .corpus/library, into .corpus/sources, and writes .corpus/closure.txt (one package per line).
# Binary installs carry no tests/ directory, so the package test harness (`oracle packages`)
# runs each package's tests from these sources. See
# docs/src/content/docs/contributing/corpus-and-oracle.md.
#
# Environment: RY_CORPUS_DIR and RY_CORPUS_SNAPSHOT, as for install.R.

args <- commandArgs(trailingOnly = FALSE)
script <- sub("^--file=", "", grep("^--file=", args, value = TRUE))
repo_root <- normalizePath(file.path(dirname(script), "..", ".."))
corpus_dir <- Sys.getenv("RY_CORPUS_DIR", file.path(repo_root, ".corpus"))
snapshot <- Sys.getenv("RY_CORPUS_SNAPSHOT", "2025-03-01")
library_dir <- file.path(corpus_dir, "library")
sources_dir <- file.path(corpus_dir, "sources")
dir.create(sources_dir, recursive = TRUE, showWarnings = FALSE)
options(timeout = 600)

roots <- c(readLines(file.path(repo_root, "tools", "corpus", "roots.txt")), "esgtoolkit", "loadfast")
installed <- installed.packages(lib.loc = c(library_dir, .Library), noCache = TRUE)
base <- rownames(installed.packages(priority = "base"))

# The closure: the roots and everything they need to load (Depends, Imports, LinkingTo).
closure <- tools::package_dependencies(
  intersect(roots, rownames(installed)), db = installed,
  which = c("Depends", "Imports", "LinkingTo"), recursive = TRUE
)
closure <- sort(setdiff(unique(c(intersect(roots, rownames(installed)), unlist(closure))), base))
writeLines(closure, file.path(corpus_dir, "closure.txt"))

repositories <- c(
  sprintf("https://packagemanager.posit.co/cran/%s", snapshot),
  "https://packagemanager.posit.co/cran/latest",
  "https://ak-finccam.github.io/r-packages"
)
available <- lapply(repositories, function(repository) {
  tryCatch(available.packages(repos = repository, type = "source"), error = function(e) NULL)
})

fetch <- function(package) {
  version <- installed[package, "Version"]
  tarball <- file.path(sources_dir, sprintf("%s_%s.tar.gz", package, version))
  if (file.exists(tarball)) return("present")
  # A GitHub remote install keeps its checkout in .corpus/src.
  checkout <- file.path(corpus_dir, "src", package)
  if (dir.exists(checkout)) {
    status <- system2("tar", c("-czf", tarball, "-C", dirname(checkout), "--exclude=.git", package))
    return(if (status == 0) "checkout" else "tar failed")
  }
  urls <- character()
  for (index in seq_along(repositories)) {
    db <- available[[index]]
    if (!is.null(db) && package %in% rownames(db) && db[package, "Version"] == version) {
      urls <- c(urls, sprintf("%s/src/contrib/%s_%s.tar.gz", repositories[[index]], package, version))
    }
  }
  urls <- c(urls, sprintf("https://cloud.r-project.org/src/contrib/Archive/%s/%s_%s.tar.gz", package, package, version))
  for (url in urls) {
    ok <- tryCatch(download.file(url, tarball, quiet = TRUE, mode = "wb") == 0, error = function(e) FALSE, warning = function(w) FALSE)
    if (ok) return(url)
    unlink(tarball)
  }
  "not found"
}

outcome <- vapply(closure, fetch, "")
missing <- names(outcome)[outcome == "not found" | outcome == "tar failed"]
cat(sprintf("closure: %d packages; sources: %d present\n", length(closure), length(closure) - length(missing)))
if (length(missing) > 0) cat("missing sources:", missing, "\n")
