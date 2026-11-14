# Two closures inlined in one compiled loop share the register window the loop reserves for
# its callees. When compiled code leaves in the middle of one of them, tier 0 must find that
# callee's locals unbound, whatever the other callee left in the window.

total <- 100
wide <- function(a, flag) {
  r <- a * 2
  if (flag) total <- r
  total + r
}
narrow <- function(a) {
  b <- a + 1
  if (b > 1e9) b <- 0
  b * 2
}
loop <- function(n, at) {
  s <- 0
  i <- 0
  while (i < n) {
    i <- i + 1
    if (i == at) total <<- 7L
    s <- s + wide(i, FALSE) + narrow(i)
  }
  s
}
for (k in 1:400) loop(50, 0)
print(loop(50, 0))
print(loop(20000, 0))
print(loop(50, 30))
total <- 100

# The callee that leaves is the one with fewer locals.
scale <- 3
few <- function(a) {
  if (a < 0) scale <- 0
  a * scale
}
many <- function(a, b) {
  x <- a + b
  y <- x * 2
  z <- y - a
  if (z > 1e9) z <- 0
  x + y + z
}
other <- function(n, at) {
  s <- 0
  i <- 0
  while (i < n) {
    i <- i + 1
    if (i == at) scale <<- 2L
    s <- s + many(i, 1) + few(i)
  }
  s
}
for (k in 1:400) other(50, 0)
print(other(50, 0))
print(other(20000, 0))
print(other(50, 30))
scale <- 3
