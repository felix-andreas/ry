# Splits a package's first load into the time its top-level S4 calls take, by function, and
# everything else. Run it as `PKG=<root> Rscript s4_profile.R` on GNU R, or under `ry run`
# from a directory that is not a package (`PKG=<root> ry run s4_profile.R`) on ry's runtime,
# where the load is ry's own loader with S4 run eagerly, so the split shows what it costs.
# A call made inside another one (setMethod creating a generic) counts toward the outer one.
root <- normalizePath(Sys.getenv("PKG"))
categories <- c("setClass", "setGeneric", "setMethod", "setClassUnion", "setValidity",
                "setOldClass", "setRefClass")
spent <- new.env()
active <- new.env()
active$depth <- 0L
for (name in categories) {
  spent[[name]] <- 0
  spent[[paste0(name, "_n")]] <- 0L
  enter <- function() {
    if (active$depth == 0L) active$start <- proc.time()[["elapsed"]]
    active$depth <- active$depth + 1L
  }
  leave <- local({
    category <- name
    function() {
      active$depth <- active$depth - 1L
      if (active$depth == 0L) {
        spent[[category]] <- spent[[category]] + proc.time()[["elapsed"]] - active$start
        count <- paste0(category, "_n")
        spent[[count]] <- spent[[count]] + 1L
      }
    }
  })
  suppressMessages(trace(name, tracer = as.call(list(enter)), exit = as.call(list(leave)),
                         where = asNamespace("methods"), print = FALSE))
}
options(keep.source = TRUE)
load <- if (exists("load_all", mode = "function") && Sys.getenv("LOADER") != "pkgload") {
  function() {
    options(ry.load.lazy_s4 = FALSE)
    load_all(root, quiet = TRUE)
  }
} else if (requireNamespace("pkgload", quietly = TRUE)) {
  function() pkgload::load_all(root, quiet = TRUE)
} else {
  # Without pkgload, a bare namespace that imports everything its imports export and sources
  # the files in collation order, which is what load_all does for this package.
  function() {
    description <- read.dcf(file.path(root, "DESCRIPTION"))
    name <- description[1L, "Package"]
    files <- if ("Collate" %in% colnames(description)) {
      scan(text = description[1L, "Collate"], what = "", quiet = TRUE)
    } else {
      sort(basename(list.files(file.path(root, "R"), pattern = "[.][Rr]$")))
    }
    imports <- if ("Imports" %in% colnames(description)) {
      sub("[ (].*", "", trimws(strsplit(description[1L, "Imports"], ",")[[1L]]))
    } else {
      character()
    }
    info <- new.env(hash = TRUE, parent = baseenv())
    parent <- new.env(hash = TRUE, parent = .BaseNamespaceEnv)
    attr(parent, "name") <- paste0("imports:", name)
    for (package in imports) {
      exports <- asNamespace(package)
      for (export in getNamespaceExports(package)) {
        assign(export, get(export, envir = exports), envir = parent)
      }
    }
    namespace <- new.env(hash = TRUE, parent = parent)
    assign(".__NAMESPACE__.", info, envir = namespace)
    info$spec <- c(name = name, version = description[1L, "Version"])
    info$exports <- new.env(hash = TRUE, parent = baseenv())
    info$imports <- list(base = TRUE)
    info$path <- root
    info$dynlibs <- character()
    info$S3methods <- matrix(NA_character_, 0L, 4L)
    assign(".packageName", name, envir = namespace)
    .Internal(registerNamespace(name, namespace))
    for (file in files) sys.source(file.path(root, "R", file), envir = namespace, keep.source = TRUE)
  }
}
total <- system.time(suppressMessages(load()))[["elapsed"]]
s4 <- 0
for (name in categories) {
  if (spent[[paste0(name, "_n")]] == 0L) next
  s4 <- s4 + spent[[name]]
  cat(sprintf("%-14s %5d calls %8.0f ms\n", name, spent[[paste0(name, "_n")]], spent[[name]] * 1000))
}
cat(sprintf("%-14s %20.0f ms\n%-14s %20.0f ms\n", "other", (total - s4) * 1000, "total", total * 1000))
