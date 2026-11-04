# Runs the probe of a package from `s4_package.py` and saves its result, so that loads with S4
# held back, run eagerly, and in GNU R can be compared.
#
#   PKG=pkg OUT=lazy.rds ry run s4_check.R
#   PKG=pkg OUT=eager.rds LAZY=FALSE ry run s4_check.R
#   PKG=pkg OUT=gnur.rds LOADER=pkgload Rscript s4_check.R
#   PKG=pkg COMPARE="eager.rds lazy.rds gnur.rds" Rscript s4_check.R
compare <- Sys.getenv("COMPARE")
if (nzchar(compare)) {
  files <- strsplit(compare, " ", fixed = TRUE)[[1]]
  reference <- readRDS(files[[1]])
  for (file in files[-1]) {
    other <- readRDS(file)
    same <- identical(reference, other)
    cat(sprintf("%s vs %s: %s\n", files[[1]], file, if (same) "identical" else "DIFFERENT"))
    if (!same) {
      for (name in union(names(reference), names(other))) {
        if (!identical(reference[[name]], other[[name]])) cat("  differs:", name, "\n")
      }
    }
  }
  quit(save = "no")
}
package <- Sys.getenv("PKG")
options(ry.load.lazy_s4 = !identical(Sys.getenv("LAZY"), "FALSE"))
if (exists("load_all", mode = "function") && !identical(Sys.getenv("LOADER"), "pkgload")) {
  load_all(package, full = TRUE)
} else {
  suppressMessages(pkgload::load_all(package, quiet = TRUE))
}
started <- proc.time()[["elapsed"]]
result <- s4_probe()
cat(sprintf("probe ran in %.0f ms\n", 1000 * (proc.time()[["elapsed"]] - started)))
saveRDS(result, Sys.getenv("OUT"))
