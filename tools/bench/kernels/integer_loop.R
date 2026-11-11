# Integer arithmetic in a while loop: Collatz chain lengths.
collatz <- function(limit) {
  total <- 0L
  for (start in 1:limit) {
    n <- start
    while (n != 1L) {
      if (n %% 2L == 0L) n <- n %/% 2L else n <- 3L * n + 1L
      total <- total + 1L
    }
  }
  total
}
measure("integer_loop", function() collatz(30000L))
