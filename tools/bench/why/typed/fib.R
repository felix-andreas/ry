# Recursion: about 2.7 million calls of a small function.
fib <- function(n: I64) -> I64 if (n < 2) n else fib(n - 1) + fib(n - 2)
measure("fib", function() Num.to_f64(fib(30)))
