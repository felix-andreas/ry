# attr() finds a full match before a partial one, and a partial match only when it is unique.
x <- 1:3
attr(x, "alpha") <- 1
attr(x, "alphabet") <- 2
attr(x, "beta") <- 3
attr(x, "al")
attr(x, "alpha")
attr(x, "alph")
attr(x, "alphab")
attr(x, "bet")
attr(x, "bet", exact = TRUE)
attr(x, "betas")
attr(x, "nam")
names(x) <- c("a", "b", "c")
attr(x, "n")
attr(x, "nam")
y <- 1:2
attr(y, "nb") <- "nb"
names(y) <- c("p", "q")
attr(y, "n")
z <- 1:2
attr(z, "nb") <- "nb"
attr(z, "n")
# A vector used as a map from fixed-width keys.
counts <- integer(0)
for (key in c("ab", "cd", "ab", "ef", "cd", "ab")) {
  count <- attr(counts, key)
  if (is.null(count)) count <- 0L
  attr(counts, key) <- count + 1L
}
attributes(counts)
attr(counts, "a")
attr(counts, "abc")
