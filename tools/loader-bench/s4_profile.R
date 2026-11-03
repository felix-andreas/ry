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
} else {
  function() pkgload::load_all(root, quiet = TRUE)
}
total <- system.time(suppressMessages(load()))[["elapsed"]]
s4 <- 0
for (name in categories) {
  if (spent[[paste0(name, "_n")]] == 0L) next
  s4 <- s4 + spent[[name]]
  cat(sprintf("%-14s %5d calls %8.0f ms\n", name, spent[[paste0(name, "_n")]], spent[[name]] * 1000))
}
cat(sprintf("%-14s %20.0f ms\n%-14s %20.0f ms\n", "other", (total - s4) * 1000, "total", total * 1000))
