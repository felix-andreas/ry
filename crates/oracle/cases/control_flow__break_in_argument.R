# A break or next inside an argument unwinds out of the callee that forces the promise and lands
# in the caller's loop, also when the loop and the callee run as compiled code.
f <- function(v) v
g <- function(m) { s <- 0; for (i in 1:m) s <- s + f(if (i == 4) break else i); s }
for (k in 1:3) print(g(6))
h <- function(m) { s <- 0; for (i in 1:m) s <- s + f(if (i %% 2 == 0) next else i); s }
for (k in 1:3) print(h(7))
inner <- function(n, v) { t <- 0; for (j in seq_len(n)) { if (j == 2) next; t <- t + j }; t + v }
outer <- function(m) {
  s <- 0
  for (i in 1:m) s <- s + inner(i, if (i == 4) break else if (i == 2) next else i)
  s
}
for (k in 1:3) print(outer(6))
w <- function(m) { s <- 0; while (TRUE) { m <- m - 1; s <- s + f(if (m < 0) break else m) }; s }
for (k in 1:3) print(w(5))
r <- function(m) { i <- 0; repeat { i <- i + 1; f(if (i > m) break else i) }; i }
for (k in 1:3) print(r(4))
