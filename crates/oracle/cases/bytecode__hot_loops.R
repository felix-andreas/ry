sum_squares <- function(n) { s <- 0; for (i in seq_len(n)) s <- s + i * i; s }
for (k in 1:3) print(sum_squares(100))
collatz <- function(n) { steps <- 0L; while (n != 1) { if (n %% 2 == 0) n <- n / 2 else n <- 3 * n + 1; steps <- steps + 1L }; steps }
vapply(c(6, 7, 27), collatz, integer(1))
fill <- function(n) { v <- numeric(n); for (i in 1:n) v[i] <- i / 2; v }
fill(5); fill(5)
shadow <- function() { `+` <- function(a, b) paste(a, b); 1 + 2 }
shadow(); shadow(); shadow()
breaky <- function() { out <- c(); for (i in 1:10) { if (i %% 3 == 0) next; if (i > 8) break; out <- c(out, i) }; out }
breaky(); breaky()
overflow <- function() { x <- .Machine$integer.max; x + 1L }
overflow(); overflow()
na_logic <- function(a) { if (is.na(a) || a) "na or true" else "false" }
na_logic(NA); na_logic(FALSE); na_logic(TRUE)
nested <- function() { total <- 0; for (i in 1:3) for (j in 1:3) { if (j == 2) next; total <- total + i * j }; total }
nested(); nested()
grow <- function(n) { x <- numeric(0); for (i in 1:n) x[i] <- i * 2; x }
for (k in 1:3) print(grow(5))
skip <- function(n) { s <- 0; for (i in 1:n) { s <- s + 1; if (i %% 2 == 0) switch("a", a = next) }; s }
skip(10); skip(10); skip(3000)
watched <- function(n) {
  x <- 0L; seen <- integer(0)
  h <- function(w) { seen <<- c(seen, x); x <<- x + 1000L; invokeRestart("muffleWarning") }
  for (i in 1:n) withCallingHandlers({ x <- i * 10L; y <- 2147483647L + i }, warning = h)
  list(x, seen)
}
for (k in 1:3) print(watched(3))
roots <- function(n) { x <- 1; for (i in 1:n) { x <- x + 1; y <- sqrt(-x) }; x }
for (k in 1:3) withCallingHandlers(print(roots(2)), warning = function(w) invokeRestart("muffleWarning"))
# A scalar loaded on one branch is boxed where it meets a vector from the other; an element write
# into that box must reach the frame.
boxed_join <- function(x, flag) { if (flag) s <- x + 1 else x <- c(5, 6); x[1] <- 9; get("x") }
for (k in 1:6) print(boxed_join(1, k %% 2 == 0))
