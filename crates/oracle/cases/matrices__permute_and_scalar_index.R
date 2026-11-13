# aperm(), t() and scalar array indexing agree with an element-by-element reference for every
# element type, rank, permutation and zero-extent shape, and keep dim names and dimnames.
reference <- function(a, perm) {
  d <- dim(a)
  nd <- d[perm]
  out <- a[0][seq_len(prod(d))]
  if (is.list(a)) out <- vector("list", prod(d))
  for (k in seq_len(prod(d))) {
    idx <- arrayInd(k, nd)
    src <- integer(length(d))
    src[perm] <- idx
    out[[k]] <- a[[sum((src - 1) * cumprod(c(1, d[-length(d)]))) + 1]]
  }
  dim(out) <- nd
  out
}

check <- function(a) {
  rank <- length(dim(a))
  perms <- if (rank == 2) list(c(2, 1)) else if (rank == 3) list(c(2, 1, 3), c(3, 1, 2), c(1, 3, 2), c(3, 2, 1), c(1, 2, 3)) else list(c(4, 2, 1, 3))
  all(vapply(perms, function(p) identical(aperm(a, p), reference(a, p)), NA))
}

set.seed(1)
shapes <- list(c(3, 5), c(70, 33), c(2, 3, 4), c(33, 40, 3), c(1, 5, 2), c(5, 1, 7), c(2, 3, 2, 4))
for (shape in shapes) {
  n <- prod(shape)
  arrays <- list(
    array(rnorm(n), shape),
    array(sample(c(1:n, NA), n), shape),
    array(sample(c(TRUE, FALSE, NA), n, TRUE), shape),
    array(as.character(seq_len(n)), shape),
    array(as.list(seq_len(n)), shape),
    array(complex(real = seq_len(n), imaginary = 1), shape),
    array(as.raw(seq_len(n) %% 256), shape)
  )
  print(vapply(arrays, check, NA))
}

# Zero extents keep their dimensions.
dim(aperm(array(0, c(2, 0, 3)), c(3, 1, 2)))
dim(t(matrix(numeric(0), 0, 4)))
t(matrix(character(0), 3, 0))

# Dimnames, their names and the names of dim move with the dimensions.
a <- array(1:24, c(2, 3, 4), dimnames = list(r = c("a", "b"), c = NULL, k = letters[1:4]))
b <- aperm(a, c(3, 1, 2))
dimnames(b)
dim(b)
b["c", "b", 3] == a["b", 3, "c"]
names(dimnames(aperm(a, c("k", "c", "r"))))
aperm(a, resize = FALSE)[1:3]
dim(aperm(a, resize = FALSE))

# t() keeps names as a row, other attributes, and dimnames.
v <- c(x = 1, y = 2, z = 3)
t(v)
m <- matrix(1:6, 2, dimnames = list(g = c("p", "q"), h = c("u", "v", "w")))
attr(m, "note") <- "kept"
t(m)
t(t(m))

# Scalar indexing reads one element, truncates a fractional position, and checks bounds.
x <- array(as.numeric(1:60), c(3, 4, 5))
x[3, 4, 5]
x[2L, 3L, 4L]
x[2.9, 1, 1]
tryCatch(x[4, 1, 1], error = conditionMessage)
x[0, 1, 1]
x[NA_integer_, 1, 1]
y <- array(c("a", "b", "c", "d"), c(2, 2))
y[2, 1]
z <- array(1:8, c(2, 2, 2), dimnames = list(c("a", "b"), NULL, NULL))
z[2, 1, 1]
w <- structure(array(1:8, c(2, 2, 2)), extra = 1)
w[1, 2, 2]
f <- function(a) { s <- 0; for (i in 1:3) for (j in 1:4) for (k in 1:5) s <- s + a[i, j, k]; s }
f(x)
f(x)
