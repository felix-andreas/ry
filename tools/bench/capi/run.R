# Times tight loops over the C API accessors and a loop that drops unprotected temporaries.
# Install the package first with the R home under test, then run this script with its Rscript:
#   R CMD INSTALL -l <library> tools/bench/capi/capibench
#   R_LIBS=<library> Rscript tools/bench/capi/run.R
library(capibench)
time <- function(label, expr) {
  gc()
  t <- system.time(expr)[["elapsed"]]
  cat(sprintf("%-12s %8.3f s\n", label, t))
}
ints <- seq_len(1e6)
reals <- as.double(ints)
strs <- as.character(ints %% 1000)
lst <- lapply(1:1e5, function(i) i)
pl <- as.pairlist(setNames(as.list(1:1e5), paste0("n", 1:1e5)))
time("INTEGER", bench("integer", ints, 50L))
time("REAL", bench("real", reals, 50L))
time("STRING_ELT", bench("string", strs, 20L))
time("VECTOR_ELT", bench("list", lst, 200L))
time("CAR/CDR", bench("pairlist", pl, 200L))
time("temporaries", bench("temporaries", 3e6))
time("mkChar", bench("strings", 3e6))
mem <- gc()
cat(sprintf("max used Mb %.1f\n", sum(mem[, ncol(mem)])))
