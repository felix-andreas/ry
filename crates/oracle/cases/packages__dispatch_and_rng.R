# is.atomic(NULL) is TRUE before R 4.4.0, the version ry reports, and base R's var() relies on it.
is.atomic(NULL)
identical(is.atomic(NULL), TRUE)

# rbind and cbind dispatch S3 methods with the ... arguments only, never deparse.level, and
# the first classed argument with a method picks it.
rbind.foo <- function(..., deparse.level = 7) list(names(list(...)), length(list(...)), deparse.level)
a <- structure(1, class = "foo")
rbind(a, z = a, deparse.level = 0)
cbind.foo <- rbind.foo
cbind(a, 2)
rbind.bar <- function(...) "bar"
b <- structure(2, class = "bar")
rbind(a, b)
rbind(1, b, a)

# The corrected Kinderman-Ramage normal generator.
RNGkind("Mersenne-Twister", "Kinderman-Ramage")
set.seed(42)
x <- rnorm(5000)
format(x[1:5], digits = 15)
format(sum(x), digits = 12)
RNGkind("default", "default")
