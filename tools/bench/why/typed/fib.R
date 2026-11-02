# Recursion: about 2.7 million calls of a small function.
fib <- function(n: double) -> double if (n < 2) n else fib(n - 1) + fib(n - 2)
measure("fib", function() fib(30))
