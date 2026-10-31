# Records GNU R's answers to the numerics conformance cases.
#
#   Rscript numerics.R <numerics.so built by R CMD SHLIB> <output file>
#
# Every case runs after set.seed(1). Lines are tab-separated: "case <name>" starts a case,
# "value <token>" is one result, "print <line>" one line the case printed, and
# "pretty <lo> <up> <n> <min.n> <shrink> <high.u.bias> <u5.bias> <f.min> <eps.correct> <bounds>
# <lo'> <up'> <n'>" one call of R_pretty through pretty()'s internal.
args <- commandArgs(trailingOnly = TRUE)
dyn.load(args[[1]])
lines <- character()
for (name in .Call("num_cases")) {
  set.seed(1)
  result <- NULL
  printed <- capture.output(result <- .Call("num_case", name))
  lines <- c(lines, paste0("case\t", name), paste0("value\t", result))
  if (length(printed)) lines <- c(lines, paste0("print\t", printed))
}
specs <- list(
  c(0, 1, 5), c(-3.7, 12.2, 5), c(1e-300, 2e-300, 5), c(0, 0, 5), c(1234.5, 1234.6, 7),
  c(-1e10, 1e10, 3), c(0.001, 0.0011, 10), c(5, 5, 5), c(-0.5, 101, 4), c(17, 1e6, 12),
  c(-2.5e-7, 3.1e-7, 6), c(0.3, 0.7, 1)
)
# A broad grid: tiny, huge, negative and zero-width ranges against several cell counts.
ends <- c(-1.7e308, -1e300, -123456.789, -10, -1, -0.02, -1e-200, -1e-310, 0, 1e-310, 3e-8,
          0.1, 1, 1.0000001, 9.99, 100, 2^52, 7.3e15, 1e300, 1.7e308)
for (low in ends) for (high in ends) if (low <= high) for (n in c(1, 2, 5, 11)) {
  specs[[length(specs) + 1L]] <- c(low, high, n)
}
defaults <- function(spec) {
  n <- spec[[3]]
  c(spec, n %/% 3, 0.75, 1.5, 0.5 + 1.5 * 1.5, 2^-20, 0, 1)
}
specs <- lapply(specs, defaults)
# Every tuning argument, over ranges that reach each branch.
for (low in c(-50, -1e-9, 0, 0.3, 1)) for (high in c(0, 1e-12, 1.0001, 99)) if (low <= high)
  for (n in c(1, 5, 13)) for (min_n in unique(c(0, 1, n))) for (high_bias in c(0, 0.5, 3, 20))
    for (five_bias in c(0.5, 2.75, 4)) for (eps in 0:2) for (bounds in 0:1)
      specs[[length(specs) + 1L]] <- c(low, high, n, min_n, if (eps == 1) 0.25 else 0.75,
                                       high_bias, five_bias, c(1e-20, 2^-20, 1)[[eps + 1]], eps,
                                       bounds)
for (spec in specs) {
  n <- as.integer(spec[[3]])
  bounds <- spec[[10]] == 1
  z <- suppressWarnings(.Internal(pretty(spec[[1]], spec[[2]], n, as.integer(spec[[4]]),
                                         spec[[5]], spec[6:8], as.integer(spec[[9]]), bounds)))
  lines <- c(lines, sprintf(
    "pretty\t%.17g\t%.17g\t%d\t%d\t%.17g\t%.17g\t%.17g\t%.17g\t%d\t%d\t%.17g\t%.17g\t%d",
    spec[[1]], spec[[2]], n, as.integer(spec[[4]]), spec[[5]], spec[[6]], spec[[7]], spec[[8]],
    as.integer(spec[[9]]), as.integer(bounds), if (bounds) z$l else z$ns,
    if (bounds) z$u else z$nu, z$n))
}
writeLines(lines, args[[2]])
