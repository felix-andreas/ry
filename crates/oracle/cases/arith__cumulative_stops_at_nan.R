# The running sums and products stop at the first NaN or NA element: every later total is NaN,
# and from the first NA on it is NA. A NaN the arithmetic makes (Inf - Inf) keeps the totals
# running. The bits show which NaN each total is.
bits <- function(x) paste(rev(as.integer(writeBin(x, raw(), endian = "little"))), collapse = ".")
show <- function(x) print(vapply(x, bits, ""))
cumsum(c(1, NaN, NA, 2))
cumsum(c(1, NA, NaN, 2))
cumsum(c(NaN, 1))
cumsum(c(Inf, -Inf, 1, NA))
show(cumsum(c(Inf, -Inf, 1)))
show(cumsum(c(1, NaN, 3)))
cumprod(c(2, 3, NaN, 4))
cumprod(c(0, Inf, 2))
show(cumprod(c(0, Inf, 2)))
cumsum(numeric(0))
cumprod(numeric(0))
cumsum(c(1e16, 1, 1, -1e16))
cumprod(c(1e300, 1e10, 1e-300))
x <- c(seq(0.1, 3, by = 0.1), NA, 1:5)
cumsum(x)
cumprod(x)
long <- rep(c(0.1, 0.2, 0.3), 100)
sum(cumsum(long) == cumsum(long[1:300]))
cumsum(c(long, NaN, long))[c(299, 300, 301, 302)]
