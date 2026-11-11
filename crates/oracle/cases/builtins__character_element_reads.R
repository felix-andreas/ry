# x[[i]] and x[i] on character vectors inside compiled loops.

lookup <- function(regions, n) {
  out <- character(n)
  for (i in seq_len(n)) out[[i]] <- regions[[(i - 1L) %% length(regions) + 1L]]
  out
}
print(lookup(c("north", "south", NA, "east"), 6))

single <- function(x) {
  s <- character(0)
  for (i in seq_along(x)) s <- c(s, x[i])
  s
}
print(single(c("a", "b")))
print(single(c(first = "a", second = "b")))

double_drops_names <- function(x) {
  r <- list()
  for (i in seq_along(x)) r[[i]] <- x[[i]]
  r
}
str(double_drops_names(c(first = "a", second = NA)))
named <- function(x) {
  r <- character(0)
  for (k in 1:2) r <- c(r, x[["second"]])
  r
}
print(named(c(first = "a", second = "b")))

utf8 <- function(x) {
  s <- NULL
  for (i in seq_along(x)) s <- c(s, nchar(x[[i]]), Encoding(x[[i]]))
  s
}
print(utf8(c("été", "abc")))

factors <- function(f) {
  r <- list()
  for (i in 1:2) r[[i]] <- f[[i]]
  r
}
str(factors(factor(c("x", "y"))))

errors <- function(x) {
  out <- list()
  out[[1]] <- tryCatch(x[[3]], error = function(e) conditionMessage(e))
  out[[2]] <- x[3]
  out[[3]] <- tryCatch(x[[NA_integer_]], error = function(e) conditionMessage(e))
  out
}
str(errors(c("a", "b")))
