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
