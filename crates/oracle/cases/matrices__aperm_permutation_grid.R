# aperm() matches a reference built with index arithmetic for every permutation of arrays of rank
# 1 to 5, every vector type, named dimnames, resize = TRUE and FALSE, and a zero extent. Each line
# lists the types that disagree, then whether all of them agree.
perms <- function(n) {
  if (n == 1) return(list(1L))
  do.call(c, lapply(seq_len(n), function(i) lapply(perms(n - 1), function(p) c(i, p + (p >= i)))))
}

reference <- function(a, perm, resize) {
  d <- dim(a)
  target <- arrayInd(seq_len(prod(d)), d[perm])
  source <- target
  source[, perm] <- target
  strides <- cumprod(c(1, d))[seq_along(d)]
  out <- a[drop((source - 1) %*% strides) + 1]
  if (resize) {
    dim(out) <- d[perm]
    if (!is.null(dimnames(a))) dimnames(out) <- dimnames(a)[perm]
  } else {
    dim(out) <- d
  }
  out
}

check <- function(a) {
  rank <- length(dim(a))
  ok <- vapply(perms(rank), function(p) {
    identical(aperm(a, p), reference(a, p, TRUE)) &&
      identical(aperm(a, p, resize = FALSE), reference(a, p, FALSE)) &&
      identical(aperm(a, names(dimnames(a))[p]), reference(a, p, TRUE))
  }, NA)
  all(ok) && identical(aperm(a), reference(a, rev(seq_len(rank)), TRUE))
}

set.seed(2)
shapes <- list(5L, c(3L, 4L), c(2L, 3L, 4L), c(3L, 1L, 2L, 2L), c(0L, 3L, 2L), c(2L, 2L, 2L, 2L, 3L))
for (dims in shapes) {
  n <- prod(dims)
  vals <- list(
    real = rnorm(n), int = sample(100L, n, TRUE), lgl = sample(c(TRUE, FALSE, NA), n, TRUE),
    chr = sample(c(letters, NA), n, TRUE), cplx = complex(real = rnorm(n), imaginary = rnorm(n)),
    raw = as.raw(sample(0:255, n, TRUE)), list = as.list(seq_len(n))
  )
  result <- vapply(vals, function(v) {
    dim(v) <- dims
    dimnames(v) <- setNames(
      lapply(seq_along(dims), function(i) if (dims[i] > 0) paste0(LETTERS[i], seq_len(dims[i]))),
      paste0("d", seq_along(dims))
    )
    check(v)
  }, NA)
  cat(format(paste(dims, collapse = "x"), width = 9), names(result)[!result], all(result), "\n")
}

# A compact integer sequence given a dim, and arrays without dimnames.
x <- 1:24
dim(x) <- c(2L, 3L, 4L)
identical(aperm(x, c(3, 1, 2)), reference(x, c(3, 1, 2), TRUE))
aperm(array(1:6, 2:3))
aperm(array(letters[1:6], c(1, 2, 3)), c(3, 2, 1))[, , 1]

# Expression vectors are not supported, as in GNU R.
e <- expression(a, b, c, d)
dim(e) <- c(2L, 2L)
tryCatch(aperm(e), error = conditionMessage)
tryCatch(t(e), error = conditionMessage)
