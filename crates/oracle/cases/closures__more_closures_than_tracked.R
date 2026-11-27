# A slot frame that defines more closures than it tracks in its slots keeps one environment, so
# the closures made before and after the limit read the same variables.
a <- function() {
  x <- 1
  holder <- new.env()
  for (i in 1:20) holder[[as.character(i)]] <- function() x
  x <- 5
  first <- holder[["1"]]
  last <- holder[["20"]]
  c(first(), last(), identical(environment(first), environment(last)))
}
print(a())
b <- function() {
  x <- 1
  ks <- list()
  for (i in 1:20) ks[[i]] <- function() x
  x <- 7
  c(ks[[1]](), ks[[20]](), identical(environment(ks[[1]]), environment(ks[[20]])))
}
print(b())
d <- function() {
  x <- 1
  for (i in 1:20) (function() x <<- x + 1)()
  k <- function() x
  x <- x * 2
  c(x, k())
}
print(d())
