# Edge cases of the vectorized paths behind subsetting, subassignment, rounding, NA tests,
# any/all and rep: each must agree with GNU R's element-by-element rules.
s <- c("a", NA, "c")
s[c(3L, 1L, NA, 5L)]
s[c(2.9, 1.1, NA)]
s[c(TRUE, NA, FALSE)]
l <- list(1, "b", NULL)
str(l[c(3L, 1L, 4L)])
str(l[c(TRUE, FALSE, TRUE)])
x <- c(1, 2, 3, 4)
x[c(4L, 1L)] <- c(40, 10)
x
x[c(1.7, 2.2)] <- 0
x
x[c(1L, 2L, 3L, 4L)] <- c(7, 8)
x
y <- 1:3
y[c(1L, 3L)] <- 9L
y
z <- c("p", "q", "r")
z[c(3L, 3L, 1L)] <- c("one", "two", "three")
z
w <- list(1, 2)
w[c(2L, 1L)] <- list("b", "a")
str(w)
v <- 1:3
v[c(1L, 2L)] <- c(1.5, 2.5)
v
u <- c(1, 2)
u[c(1L, 2L, 1L)] <- c(5, 6, 7)
u
withCallingHandlers({ t <- 1:4; t[c(1L, 2L, 3L)] <- c(0L, 1L); print(t) },
  warning = function(w) { cat("warning:", conditionMessage(w), "\n"); invokeRestart("muffleWarning") })
floor(c(-1.5, 2.5, NA, NaN, Inf))
ceiling(c(-1.5, 2.5, NA, NaN, -Inf))
trunc(c(-1.5, 2.5, NA))
abs(c(-1.5, NA, -Inf))
withCallingHandlers(sqrt(c(4, -1, NA)),
  warning = function(w) { cat("warning:", conditionMessage(w), "\n"); invokeRestart("muffleWarning") })
floor(structure(c(a = 1.5, b = -0.5), extra = "kept"))
is.na(c(1, NA, NaN, Inf))
is.nan(c(1, NA, NaN, Inf))
is.finite(c(1L, NA))
is.infinite(c(1, -Inf, NA))
is.na(c(TRUE, NA))
any(c(0L, NA, 2L))
any(c(0L, NA))
any(c(0L, NA), na.rm = TRUE)
all(c(1L, NA, 0L))
all(c(1L, NA))
all(c(TRUE, TRUE), c(5L, NA), na.rm = TRUE)
any(logical(0))
all(logical(0))
rep.int(c(1.5, NA), 3)
rep.int(c(TRUE, NA), 2)
rep.int(1:2, 0)
rep_len(c(1L, 2L, 3L), 7L)
rep_len(c(0.5, 1), 1L)
rep.int(factor(c("a", "b")), 2)
