`[<-.loc` <- function(x, i, j, value) {
  cat("sub:", deparse(substitute(i)), if (!missing(j)) deparse(substitute(j)), "missing i:", missing(i), "nargs:", nargs(), "\n")
  print(sys.call())
  unclass(x)
}
`[[<-.loc` <- function(x, i, value) {
  cat("sub2:", deparse(substitute(i)), "\n")
  unclass(x)
}
f <- function() {
  k <- 2L
  z <- structure(list(1, 2), class = "loc")
  z[k] <- 5
  z <- structure(list(1, 2), class = "loc")
  z[[k]] <- 5
  z <- structure(list(1, 2), class = "loc")
  z[k, k] <- 5
  for (i in 1:2) {
    z <- structure(list(1, 2), class = "loc")
    z[i] <- 3
  }
  z
}
for (n in 1:3) str(f())
g <- function() {
  z <- structure(list(1, 2), class = "loc")
  r <- tryCatch({ z[late] <- 1; "ok" }, error = function(e) conditionMessage(e))
  late <- 1
  r
}
for (n in 1:3) print(g())
