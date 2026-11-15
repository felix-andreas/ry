# Complex arithmetic and math on NA, NaN and Inf in each part, recycling, mixed kinds and
# attributes, called directly and from compiled code. Each result shows both parts, so a NaN
# that should be NA (or the reverse) is visible.
quiet <- function(expr) withCallingHandlers(expr, warning = function(w) {
  cat("warning:", conditionMessage(w), "\n"); invokeRestart("muffleWarning")
})
parts <- function(z) {
  print(rbind(re = Re(z), im = Im(z)))
  if (!is.null(attributes(z))) str(attributes(z))
}
z <- c(complex(real = 1.5, imaginary = -2), NA, complex(real = NaN, imaginary = 1),
  complex(real = 1, imaginary = NaN), complex(real = Inf, imaginary = 0),
  complex(real = 0, imaginary = -Inf), complex(real = Inf, imaginary = Inf), 0, -3, 2i)
w <- rev(z)
d <- c(NA, NaN, 2, -1, Inf, 0, 0.5, -Inf, 3, 1)
n <- c(NA, 1L, 2L, -3L, 0L, 7L, NA, 1L, -1L, 2L)
b <- c(TRUE, NA, FALSE, TRUE, NA, FALSE, TRUE, TRUE, FALSE, NA)
ops <- list(`+` = `+`, `-` = `-`, `*` = `*`, `/` = `/`, `^` = `^`)
pairs <- list(zw = list(z, w), zd = list(z, d), dz = list(d, z), zn = list(z, n),
  nz = list(n, z), zb = list(z, b), zs = list(z, 2), sz = list(2 + 1i, z), zna = list(z, NA),
  nas = list(NA_complex_, z), z2 = list(z, 2), z2i = list(z, 2L), z2c = list(z, 2 + 0i),
  zneg = list(z, -2), zhalf = list(z, 0.5), short = list(z, c(1i, NA)),
  uneven = list(z, c(1 + 1i, 2, 3)))
# An NA part plus a NaN part is NA or NaN depending on which operand GNU R's C compiler put
# first, so those sums are in kernels__complex_na_nan_sums.R, which the recorded build checks.
for (op in names(ops)) for (pair in names(pairs)) {
  if (op == "+" && pair %in% c("zd", "dz", "zna", "nas")) next
  cat(op, pair, ":\n")
  parts(quiet(ops[[op]](pairs[[pair]][[1]], pairs[[pair]][[2]])))
}

# Attributes come from the longer operand, or from both when the lengths match.
m <- matrix(z[1:6], 2, dimnames = list(c("a", "b"), NULL))
parts(m * 2i); parts(2i + m); parts(m^2); parts(m / m); parts(m - z[1:6])
named <- setNames(z[1:3], c("x", "y", "w"))
parts(named * c(1i, 2, 3)); parts(c(1i, 2, 3) - named); parts(named^2)

# Compiled code reuses its buffers across iterations, so the results must not alias.
compiled <- function(a, c) {
  for (k in 1:5) a <- a^2 + c
  list(a, a * c, a / c, a - c)
}
for (k in 1:3) result <- compiled(z[1:4] / 4, c(-0.5 + 0.5i, 0.25i))
for (r in result) parts(r)
mandel <- function(n) {
  c <- outer(seq(-2, 0.5, length.out = n), 1i * seq(-1, 1, length.out = n), "+")
  z <- 0 * c
  for (k in 1:20) z <- z^2 + c
  sum(Mod(z) <= 2, na.rm = TRUE)
}
print(mandel(30))

# Math functions: NA in either part is NA, attributes are kept, and a NaN result warns.
for (f in c("sqrt", "exp", "log", "cos", "sin", "tan", "abs", "Mod", "Arg", "Conj",
  "Re", "Im", "cosh", "acos", "atan")) {
  cat(f, ":\n")
  parts(quiet(get(f)(z)))
  parts(quiet(get(f)(m)))
}
parts(sqrt(NA_complex_)); parts(exp(complex(real = NA, imaginary = 0)))
parts(-m); parts(exp(-abs(m)))
