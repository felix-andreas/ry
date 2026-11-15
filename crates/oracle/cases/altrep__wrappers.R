# Attribute changes on long vectors go through ALTREP wrappers that share the data. Each result
# must read exactly as a plain copy would, whoever holds the original.
y <- as.numeric(1:100)

# unclass() under a variable that is rebound keeps the class off, so arithmetic does not dispatch.
ym <- structure(2000 + (0:99) / 12, class = "ym")
`-.ym` <- function(e1, e2) stop("dispatched on a class unclass() removed")
f <- function(x) {
  x <- unclass(x)
  year <- floor(x + 0.001)
  head(x - year, 3)
}
f(ym)
f(ym)
class(ym)

# names<-, attr<-, structure() and dimnames<- leave the source alone.
h <- function(x) { names(x) <- paste0("n", seq_along(x)); x }
w <- h(y)
c(class(w), length(w), names(w)[3])
is.null(names(y))
k <- function(x) { attr(x, "a") <- 1; x }
attributes(k(y))
attributes(y)
s <- structure(y, class = "foo")
u <- unclass(s)
c(class(s), class(u), is.object(u))
m <- matrix(as.numeric(1:200), 100)
d <- m
dimnames(d) <- list(NULL, c("a", "b"))
dimnames(m)

# Writing into a wrapped copy never writes through to the source.
g <- function(x) { names(x) <- NULL; x[1] <- -1; x }
head(g(y), 2)
head(y, 2)
v <- k(y)
v[2] <- 99
c(v[2], y[2])

# An assignment that selects nothing keeps the value and its type.
z <- function(x) { x[FALSE] <- 1; x }
r <- z(1:100)
c(typeof(r), length(r), r[100])
z2 <- function(x) { x[integer(0)] <- 1L; x }
typeof(z2(as.numeric(1:100)))

# Compact sequences: duplicates are independent, and serialization is unchanged by reads.
x <- 1:10
s1 <- serialize(x, NULL)
invisible(x[[3]])
sum(x)
identical(serialize(x, NULL), s1)
x2 <- x
x2[1] <- 0L
c(x[1], x2[1])
as.numeric(1:5)
identical(as.numeric(1:70), seq(1, 70))
rev(1:3)
