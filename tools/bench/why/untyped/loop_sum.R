# A numeric loop: a scalar double accumulator in a counted loop.
loop_sum <- function(n) {
  s <- 0
  for (i in 1:n) s <- s + i * 0.5
  s
}
measure("loop_sum", function() loop_sum(10000000L))
