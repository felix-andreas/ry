# Recursion: about 250,000 calls of a small closure.
fib <- function(n) if (n < 2) n else fib(n - 1) + fib(n - 2)
measure("fib", function() fib(25))
