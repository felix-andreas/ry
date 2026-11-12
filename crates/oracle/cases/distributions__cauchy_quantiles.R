# oracle: library-dependent
# The Cauchy quantiles, and those of t with one degree of freedom, which GNU R computes with
# `tanpi`. A GNU R configured against a C library that has `tanpi` (glibc 2.41 and later) calls
# that one, otherwise its own, and the two differ in the last bit. The points and checksums are
# those of distributions__grid.R.
bits <- function(x) {
  r <- as.numeric(writeBin(as.double(x), raw()))
  sprintf("%.0f", sum(r * ((seq_along(r) %% 251) + 1)))
}
P <- c(1e-300, 1e-100, 1e-20, 1e-10, 1e-5, .001, .01, .1, .25, .5, .75, .9, .99, .999, 1 - 1e-10)
LP <- log(P)
for (case in list(list("t", 1), list("cauchy", c(0, 1)), list("cauchy", c(1, .01)),
  list("cauchy", c(-5, 100)))) {
  q <- get(paste0("q", case[[1]]))
  qv <- numeric()
  for (lt in c(TRUE, FALSE)) for (lg in c(FALSE, TRUE)) {
    qv <- c(qv, suppressWarnings(do.call(q, c(list(if (lg) LP else P), as.list(case[[2]]),
      list(lower.tail = lt, log.p = lg)))))
  }
  cat(sprintf("%-26s q %s\n", paste0(case[[1]], "(", paste(case[[2]], collapse = ","), ")"),
    bits(qv)))
}
