# Calls to small closures in a loop, one of them a closure over its defining frame.
closures <- function(n) {
  add <- function(a, b) a + b
  scale <- 3
  times_scale <- function(a) a * scale
  s <- 0
  for (i in 1:n) s <- add(s, times_scale(i))
  s
}
measure("closures", function() closures(1000000L))
