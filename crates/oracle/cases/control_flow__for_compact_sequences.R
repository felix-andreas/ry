# `for` over a compact sequence (`1:n`, `n:1`, `seq_len`, `seq_along`, a range past the
# integer limits) steps through its elements without expanding it. The loop variable's value
# must still be a fresh integer or double each time the body keeps it, the body may rebind the
# variable or the vector the sequence came from, and a loop that turns hot moves into compiled
# code mid-way. Each function runs a few times so compiled code runs it too.
total <- function(n) {
  s <- 0L
  for (i in 1:n) s <- s + i
  s
}

down <- function(n) {
  out <- integer()
  for (i in n:1) out <- c(out, i)
  out
}

kept <- function(n) {
  seen <- list()
  for (i in seq_len(n)) seen[[i]] <- i
  seen
}

rebound <- function() {
  out <- integer()
  for (i in 1:4) {
    out <- c(out, i)
    i <- i * 100L
  }
  c(out, i)
}

source_changed <- function() {
  x <- 1:5
  for (i in x) x[i] <- 0L
  x
}

wide <- function() {
  big <- 2^31
  out <- numeric()
  for (v in (big - 1):(big + 1)) out <- c(out, v)
  list(out, typeof(v))
}

nested <- function(n) {
  s <- 0
  for (i in 1:n) for (j in i:n) s <- s + i * j
  s
}

hot <- function(n) {
  s <- 0
  for (i in 1:n) {
    s <- s + i
    if (i == n - 2L) s <- s + 0.5
  }
  s
}

empty <- function() {
  count <- 0L
  for (i in seq_len(0)) count <- count + 1L
  for (i in seq_along(c("a", "b", "c"))) count <- count + i
  count
}

broken <- function() {
  last <- NA
  for (i in 10:1) {
    if (i < 7L) break
    last <- i
  }
  c(last, i)
}

for (round in 1:3) {
  print(total(100L))
  print(down(5L))
  str(kept(3L))
  print(rebound())
  print(source_changed())
  print(wide())
  print(nested(20L))
  print(hot(5000L), digits = 12)
  print(empty())
  print(broken())
}
