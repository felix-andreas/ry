# Element assignment in hot loops: a vector whose type changes partway, and lists filled by
# `[[<-` in place, by appending, through a second reference, and with names.

fill_logical <- function(n) {
  x <- vector(length = n)
  for (i in 1:n) x[[i]] <- i
  x
}
x <- fill_logical(5000)
cat(typeof(x), length(x), sum(x), x[c(1, 2500, 5000)], "\n")

fill_double_late <- function(n) {
  x <- integer(n)
  for (i in 1:n) x[i] <- if (i > n / 2) i + 0.5 else i
  x
}
x <- fill_double_late(4000)
cat(typeof(x), sum(x), x[c(1, 2000, 2001, 4000)], "\n")

fill_logical_values <- function(n) {
  x <- numeric(n)
  for (i in 1:n) x[[i]] <- i %% 3 == 0
  x
}
x <- fill_logical_values(3000)
cat(typeof(x), sum(x), "\n")

fill_named <- function(n) {
  x <- setNames(integer(n), paste0("k", 1:n))
  for (i in 1:n) x[[i]] <- i * 2L
  x
}
x <- fill_named(3000)
cat(typeof(x), sum(x), names(x)[c(1, 3000)], "\n")

na_store <- function(n) {
  x <- numeric(n)
  for (i in 1:n) x[[i]] <- if (i %% 2 == 0) NA_integer_ else i
  x
}
x <- na_store(2000)
cat(sum(is.na(x)), sum(x, na.rm = TRUE), "\n")

append_list <- function(n) {
  x <- list()
  for (i in 1:n) x[[i]] <- i
  x
}
x <- append_list(5000)
cat(length(x), typeof(x[[1]]), x[[1]], x[[5000]], sum(unlist(x)), "\n")

append_objects <- function(n) {
  x <- list()
  for (i in 1:n) x[[length(x) + 1]] <- c(i, -i)
  x
}
x <- append_objects(3000)
cat(length(x), x[[3000]], "\n")

shared_list <- function(n) {
  x <- vector("list", n)
  kept <- NULL
  for (i in 1:n) {
    if (i == n / 2) kept <- x
    x[[i]] <- i
  }
  list(x, kept)
}
r <- shared_list(4000)
cat(sum(unlist(r[[1]])), sum(unlist(r[[2]])), length(r[[2]]), "\n")

delete_by_null <- function(n) {
  x <- as.list(1:n)
  for (i in 1:(n / 2)) x[[i]] <- if (i %% 10 == 0) NULL else -i
  x
}
x <- delete_by_null(4000)
cat(length(x), x[[1]], x[[9]], x[[10]], "\n")

named_list <- function(n) {
  x <- list(a = 0)
  for (i in 1:n) x[[i + 1]] <- i
  x
}
x <- named_list(3000)
cat(length(x), names(x)[1:3], x[[3001]], "\n")

single_bracket_list <- function(n) {
  x <- vector("list", n)
  for (i in 1:n) x[i] <- list(i)
  x
}
x <- single_bracket_list(3000)
cat(typeof(x[[1]]), x[[3000]], "\n")

global_list <- as.list(1:10)
outer_copy <- function(n) {
  for (i in 1:n) global_list[[i]] <- i * 10
  global_list
}
x <- outer_copy(3000)
cat(length(x), x[[3000]], global_list[[1]], length(global_list), "\n")

gap_append <- function(n) {
  x <- list()
  for (i in 1:n) x[[2 * i]] <- i
  x
}
x <- gap_append(2000)
cat(length(x), is.null(x[[1]]), x[[4000]], "\n")

self_reference <- function(n) {
  x <- list(0, 0)
  for (i in 1:n) {
    x[[1]] <- i
    if (i %% 1000 == 0) x[[2]] <- x
  }
  x
}
x <- self_reference(3000)
cat(x[[1]], x[[2]][[1]], x[[2]][[2]][[1]], "\n")

nested_reference <- function(n) {
  x <- list(0, 0)
  for (i in 1:n) {
    x[[1]] <- i
    if (i %% 1000 == 0) x[[2]] <- list(x)
  }
  x
}
x <- nested_reference(3000)
cat(x[[1]], x[[2]][[1]][[1]], x[[2]][[1]][[2]][[1]][[1]], "\n")
