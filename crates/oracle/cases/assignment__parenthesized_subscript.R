# Element assignments whose subscript is parenthesized, warmed into compiled code.
run <- function(n) {
  x <- numeric(8)
  y <- 1:8
  z <- list(1, 2, 3)
  for (k in seq_len(n)) {
    x[[(k %% 8L) + 1L]] <- k
    y[(k %% 8L) + 1L] <- k
    z[[(k %% 3L) + 1L]] <- k * 2
  }
  x[(2)] <- NA
  y[[((3L))]] <- -1L
  list(x, y, z)
}
for (round in 1:3) str(run(20L))
h <- function() { x <- 1:3; i <- 2; x[(i <- i + 1)] <- 0L; list(x, i) }
for (round in 1:3) str(h())
