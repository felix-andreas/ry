# print() stops at `max` entries, printing whole rows and slices.
m <- matrix(1:12, 6)
print(m, max = 4)
print(matrix(1:12, 2), max = 3)
options(max.print = 6); print(matrix(1:30, 10)); options(max.print = 99999)
print(array(1:24, c(2, 3, 4)), max = 10)
# dimnames made of NULL components stay attached.
m <- matrix(1); dimnames(m) <- list(NULL, NULL); str(attributes(m))
m <- matrix(1); attr(m, "dimnames") <- list(NULL, NULL); str(attributes(m))
x <- structure(1, dim = c(1L, 1L), dimnames = list(NULL, NULL), class = "foo")
str(attributes(x[, 1, drop = FALSE]))
# Zero-length arithmetic keeps only names and array structure.
a <- structure(numeric(), tzone = "UTC", tclass = "Date")
str(attributes(a %/% 86400)); str(attributes(a + 1)); str(attributes(-a))
str(attributes(matrix(numeric(), 0, 2) + 1)); str(attributes(c(a = 1)[0] + 1))
# A matrix subscript with NA rows skips them for a single value and rejects a longer one.
x <- matrix(1:6, nrow = 3)
k <- matrix(c(NA, 1, 1, 1, 2, 2), ncol = 2, byrow = TRUE)
z <- x; z[k] <- -1; print(z)
z <- x; try(z[k] <- -(1:3)); print(z)
print(as.list(1:6), max = 3)
print(as.list(1:4), max = 3)

# A hidden row still widens its column, and one omitted row reads singular.
print(matrix(c(1, 2, 1000, 3.5), 4), max = 2)
print(matrix(1:4, 2), max = 2)
print(matrix(c(1, 2, 1000, 3.5), 2), max = 2)

# A classed rep reaches its method whatever the caller names its variables.
f <- function(d, k) rep(d, k)
f(as.Date("2020-01-01"), 2)
g <- function(d, k) rep_len(d, k)
g(as.Date("2020-01-01"), 3)

# A zero-length result takes names only from the left operand.
y <- structure(numeric(0), names = character(0))
attributes(1 + y)
attributes(y + 1)
attributes(numeric(0) + y)
