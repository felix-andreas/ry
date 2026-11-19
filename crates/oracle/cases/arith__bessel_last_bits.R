# The Bessel functions follow GNU R's netlib routines to the last bit, so each value prints exactly.
x <- c(1e-300, 1e-8, 0.1, 0.5, 1, 2, 5, 9.5, 10, 25, 49.9, 100, 1e3, 1e4, 1e5)
nu <- c(0, 0.25, 0.5, 1, 1.5, 2.75, 5, 10, 30.5, 100, -0.75, -2.5)
grid <- expand.grid(x = x, nu = nu)
for (f in list(besselJ, besselY)) print(sprintf("%a", f(grid$x, grid$nu)))
for (scaled in c(FALSE, TRUE)) {
  print(sprintf("%a", besselI(grid$x, grid$nu, expon.scaled = scaled)))
  print(sprintf("%a", besselK(grid$x, abs(grid$nu), expon.scaled = scaled)))
}
besselJ(5, 200)
besselY(1e5, 50)
besselI(matrix(1:6, 2), 1)
besselK(0, 1)
besselJ(-1, 0)
