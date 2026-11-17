# Once a running sum or product meets NaN or NA, every later total is the default NaN, and NA
# from the first NA on, whatever sign and payload the NaN carried. The bytes show it.
bits <- function(x) print(vapply(x, function(v) paste(rev(writeBin(v, raw())), collapse = ""), ""))
inputs <- list(
    c(1, NA, 2, NaN, 3),
    c(1, NaN, 2, NA, 3),
    c(NaN, NA),
    c(NA, NaN),
    c(-NaN, NaN, 1),
    c(NaN, -NaN, 1),
    c(Inf, -Inf, NA, 1),
    c(1, 2, 3, NA),
    c(NA_real_),
    c(0, 1e308, 1e308, NA, -1e308),
    rep(c(NA, 1, NaN), 30)
)
for (x in inputs) {
    print(cumsum(x)); bits(cumsum(x))
    print(cumprod(x)); bits(cumprod(x))
}
print(cumsum(c(1L, NA, 2L)))
print(cumsum(c(a = 1, b = NA, c = 3)))
mk <- function(h) readBin(rev(as.raw(strtoi(substring(h, seq(1, 15, 2), seq(2, 16, 2)), 16L))), "double")
for (h in c("fff8000000000123", "7ff0000000000123", "7ff8000000000500", "7fffffffffffffff")) {
    p <- mk(h)
    for (x in list(p, c(1, p), c(p, NA), c(NA, p), c(-NA_real_, p))) {
        bits(cumsum(x)); bits(cumprod(x))
    }
}
