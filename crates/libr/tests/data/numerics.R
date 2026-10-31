# Records GNU R's answers to the numerics conformance cases.
#
#   Rscript numerics.R <numerics.so built by R CMD SHLIB> <output file>
#
# Every case runs after set.seed(1). Lines are tab-separated: "case <name>" starts a case,
# "value <token>" is one result, "print <line>" one line the case printed, and
# "pretty <lo> <up> <n> <lo'> <up'> <n'>" one call of R_pretty through pretty()'s internal.
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
for (spec in specs) {
  n <- as.integer(spec[[3]])
  z <- .Internal(pretty(spec[[1]], spec[[2]], n, n %/% 3L, 0.75, c(1.5, 0.5 + 1.5 * 1.5, 2^-20),
                        0L, TRUE))
  lines <- c(lines, sprintf("pretty\t%.17g\t%.17g\t%d\t%.17g\t%.17g\t%d",
                            spec[[1]], spec[[2]], n, z$l, z$u, z$n))
}
writeLines(lines, args[[2]])
