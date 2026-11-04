# %*%, crossprod and tcrossprod honour options(matprod). "internal" accumulates every entry in
# long double, and the other modes take GNU R's plain double loop when an operand holds a NaN or
# an infinity. Finite operands under the BLAS modes give whatever BLAS the machine has, so they
# are not part of this case.
set.seed(2)
bits <- function(x) {
  r <- as.numeric(writeBin(as.double(x), raw()))
  sprintf("%.0f", sum(r * ((seq_along(r) %% 251) + 1)))
}
m <- matrix(runif(20 * 6) * 1e6, 20, 6)
a <- matrix(runif(7 * 40) * 1e3 - 500, 7, 40)
b <- matrix(runif(40 * 5) * 1e3 - 500, 40, 5)
v <- runif(40) * 1e3
w <- runif(7) * 1e3
loop <- matrix(0, 6, 6)
for (i in 1:6) for (j in 1:6) { s <- 0; for (k in 1:20) s <- s + m[k, i] * m[k, j]; loop[i, j] <- s }

options(matprod = "internal")
cat("internal crossprod equals a plain double loop:", identical(crossprod(m), loop), "\n")
cases <- list(
  gemm = function() a %*% b,
  gemv = function() a %*% v,
  vecmat = function() w %*% a,
  dot = function() v %*% v,
  outer = function() w %*% t(v),
  cross_matrix = function() crossprod(t(a), t(a)),
  cross_sym = function() crossprod(m),
  cross_vec = function() crossprod(v),
  cross_two = function() crossprod(m, m[, 1:3]),
  tcross_sym = function() tcrossprod(a),
  tcross_two = function() tcrossprod(a, a[1:3, ]),
  tcross_vec = function() tcrossprod(w, w),
  complex = function() (a + 1i * a[7:1, ]) %*% (b - 1i * b[40:1, ])
)
for (name in names(cases)) {
  z <- cases[[name]]()
  cat(sprintf("%-13s %s %s\n", name, paste(dim(z), collapse = "x"), bits(c(Re(z), Im(z)))))
}
print(sprintf("%a", crossprod(m)[1:3, 1:2]))

nan <- a; nan[3, 4] <- NaN
inf <- a; inf[2, 2] <- Inf
na <- b; na[5, 1] <- NA
for (mode in c("default", "default.simd", "internal")) {
  options(matprod = mode)
  cat(mode, bits(nan %*% b), bits(inf %*% b), bits(a %*% na), bits(crossprod(nan)), bits(tcrossprod(inf)), "\n")
  print((nan %*% b)[3, 1:2]); print((a %*% na)[1:2, 1]); print((inf %*% b)[2, 1:2])
}

options(matprod = "default")
r <- try(options(matprod = "bogus"), silent = TRUE)
cat(conditionMessage(attr(r, "condition")), "\n")
print(getOption("matprod"))
