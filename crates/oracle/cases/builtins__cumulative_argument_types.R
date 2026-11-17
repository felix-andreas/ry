# The cumulative functions, `which.max`/`which.min` and `pmax`/`pmin` on each argument type:
# raw, lists and strings are coerced to double, and complex numbers have no order.
probe <- function(code) {
  cat(code, "-> ")
  tryCatch(
    str(withCallingHandlers(eval(parse(text = code)), warning = function(w) {
      cat("warning:", conditionMessage(w), "| ")
      invokeRestart("muffleWarning")
    })),
    error = function(e) cat("error:", conditionMessage(e), "\n")
  )
}
r <- as.raw(c(a = 1, b = 18, c = 3))
names(r) <- c("a", "b", "c")
for (code in c(
  "cumsum(r)", "cumprod(r)", "cummax(r)", "cummin(r)", "cumsum(raw())", "cummax(raw())",
  "cumsum(list(1, 2))", "cummin(c(\"3\", \"1\"))", "cumsum(NULL)", "cumsum(sum)",
  "cummax(c(1i, 2i))", "cummin(1i)", "cummax(complex())", "cumsum(c(1i, 2i))",
  "which.max(r)", "which.min(r)", "which.max(c(1i, 2+0i))", "which.max(c(\"b\", \"a\"))",
  "which.max(list(TRUE, 2))", "which.min(list(p = 3, q = 1))",
  "pmax(c(TRUE, NA, FALSE))", "pmin(NULL)", "pmax(logical())", "pmax(c(a = 1L, b = 2L))",
  "pmax(1i)", "pmax(1, 1i)", "pmin(TRUE, FALSE)", "pmax(1, numeric(), list())",
  "Conj(c(TRUE, NA))", "Conj(matrix(1:4, 2))", "Re(factor(\"x\"))", "Conj(factor(\"x\"))",
  "sign(complex())", "floor(complex())", "log2(c(1i, 2+0i))", "log10(c(1i, 2+0i))"
)) probe(code)
f <- function(x) cumsum(x)
for (i in 1:3) probe("f(r)")
g <- function(x) which.max(x)
for (i in 1:3) probe("g(r)")
