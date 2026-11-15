# Element reads of deferred string conversions inside a loop, where each read converts only
# that element, and the conversions must match a full as.character().
reads <- function(s, n) {
  out <- character(n)
  for (i in seq_len(n)) out[i] <- s[i]
  out
}
deep <- function(s, n) {
  out <- character(n)
  for (i in seq_len(n)) out[i] <- s[[i]]
  out
}
x <- as.character((1:80) * 7L)
stopifnot(identical(reads(x, 80L), as.character((1:80) * 7L)))
print(reads(x, 5L))
y <- as.character(c(1.5, -0, 1e-20, 1e15, 123456789.123, NA, Inf, -Inf, NaN, 0.1 + 0.2))
print(reads(y, 10L))
print(deep(y, 10L))
print(y[2.9])
print(y[[3.1]])
print(y[11])
z <- as.character(c(NA_integer_, -5L, 2147483647L))
print(deep(z, 3L))
print(is.na(reads(z, 3L)))
w <- as.character(seq(0.25, 20, by = 0.25))
print(reads(w, 8L))
