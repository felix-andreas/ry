# Complex arithmetic over vectors: equal lengths, a scalar on either side, recycling with its
# warning, real, integer and logical operands converted with NA kept, zero-length operands, and
# the special values of multiplication, division and powers.
z <- complex(real = c(1, -2, 0.5, 3), imaginary = c(0, 1.5, -2, 4))
print(z * z)
print(z + 1i)
print(2i - z)
print(z / (1 + 1i))
print(z ^ 2)
print(z ^ 0.5)
print(z * 1:2)
print(z + c(1, NA, 3, 4))
print(z * c(TRUE, FALSE, NA, TRUE))
print(NA_integer_ + z)
print(z * complex(0))
print(complex(0) + 1)
print(withCallingHandlers(z + c(1i, 2i, 3i), warning = function(w) {
  cat("warning:", conditionMessage(w), "\n"); invokeRestart("muffleWarning")
}))
special <- complex(real = c(Inf, NaN, 0, -Inf), imaginary = c(1, 0, Inf, NA))
print(special * special)
print(special / c(0+0i, 1i, Inf, 2))
print(special ^ c(2, 0.5))
loop <- function(n) {
  z <- rep(0+0i, 4); c <- complex(real = c(-0.5, 0.25, 0, -1), imaginary = c(0.5, 0.25, 1, 0))
  for (i in 1:n) z <- z * z + c
  z
}
print(loop(6))
x <- structure(1:4 + 0i, names = letters[1:4], dim = c(2, 2))
print(x * x)
