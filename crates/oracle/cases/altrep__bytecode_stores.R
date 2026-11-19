# Element stores and lookups in compiled loops whose targets, subscripts or values are ALTREP
# objects. The compiled store fast paths decline them, and the full assignment must agree.
f <- function() {
  x <- 1:5
  for (i in 1:3) x[[i]] <- i * 10L
  y <- as.character(c(1, 2, 3))
  for (i in 1:3) y[[i]] <- paste0(y[[i]], "!")
  z <- sort(c(70:1 + 0.5))
  for (i in 1:2) z[i] <- -i
  l <- as.list(1:3)
  names(l) <- 1:3
  for (key in as.character(1:3)) l[[key]] <- key
  for (key in as.character(4:5)) l[[key]] <- key
  l$`2` <- "dollar"
  m <- 1:6
  dim(m) <- 2:3
  for (j in 1:3) m[2, j] <- 0L
  e <- new.env()
  for (key in as.character(c(1, 2))) assign(key, key, envir = e)
  e$`3` <- "three"
  list(x, y, z[1:3], l, m, sort(ls(e)), mget(as.character(1:3), envir = e))
}
print(f())
print(f())

g <- function(v) {
  total <- 0
  for (i in seq_along(v)) total <- total + v[[i]]
  total
}
print(g(1:100)); print(g(sort(c(70:1 + 0.25)))); print(g(seq(1, 2, by = 0.25)))

h <- function(n) {
  out <- character(0)
  for (i in seq_len(n)) out[[as.character(i)]] <- as.character(i * 2)
  out
}
print(h(3)); print(names(h(3)))
