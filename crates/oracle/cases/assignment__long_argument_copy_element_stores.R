# A function that assigns elements of a long argument (64 elements or more) copies it once on its
# first assignment and writes the rest into that copy. The function body compiles on its second
# call, so the second call is the one that exercises the compiled copy.
m0 <- matrix(0, 8, 8)
f <- function(m) {
  m[1, 1] <- 1
  m[2, 2] <- 1
  m
}
r1 <- f(m0)
r2 <- f(m0)
r3 <- f(m0)
cat(sum(r1), sum(r2), sum(r3), sum(m0), identical(r1, r3), dim(r3), "\n")

# The same through every element type, a vector subscript, and a length on each side of 64.
g_matrix <- function(m, value) {
  m[1, 1] <- value
  m[2, 2] <- value
  m[nrow(m), ncol(m)] <- value
  m
}
g_vector <- function(v, value) {
  v[1] <- value
  v[2] <- value
  v[length(v)] <- value
  v
}
g_list <- function(l, value) {
  l[[1]] <- value
  l[[2]] <- value
  l[[length(l)]] <- value
  l
}
for (n in c(7L, 8L, 9L, 16L)) {
  for (seed in list(0, 0L, FALSE, "", 0i, as.raw(0))) {
    m <- matrix(seed, n, n)
    value <- switch(typeof(seed), raw = as.raw(1), character = "x", logical = TRUE, integer = 1L, seed + 1)
    for (call in 1:3) {
      r <- g_matrix(m, value)
      v <- g_vector(as.vector(m), value)
      stopifnot(identical(m, matrix(seed, n, n)))
    }
    cat(n, typeof(r), sum(r == value), sum(v == value), "\n")
  }
  l <- as.list(seq_len(n * n))
  for (call in 1:3) r <- g_list(l, "x")
  cat(n, "list", sum(r == "x"), identical(l, as.list(seq_len(n * n))), "\n")
}

# A loop of element assignments into a long argument, called more than once, and a caller that
# keeps the original.
h <- function(m) {
  for (i in seq_len(nrow(m))) m[i, i] <- i
  m
}
big <- matrix(0, 20, 20)
for (call in 1:4) out <- h(big)
cat(sum(diag(out)), sum(big), "\n")

# An attribute on the long argument survives the copy.
a <- structure(numeric(100), extra = "kept")
k <- function(x) {
  x[1] <- 1
  x[2] <- 2
  x
}
for (call in 1:3) out <- k(a)
cat(attr(out, "extra"), sum(out), sum(a), "\n")
message("ok")
