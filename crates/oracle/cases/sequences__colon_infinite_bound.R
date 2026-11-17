# A sequence with an infinite or huge bound is an error, not an allocation.
try(-Inf:3)
try(1:-Inf)
try(1:Inf)
try(0:1e300)
r <- tryCatch(-Inf:3, error = function(e) conditionMessage(e))
r
f <- function(a, b) a:b
try(f(1, Inf))
try(f(1, Inf))
2:5
5.5:2
try(seq_len(Inf))
try(seq_len(1e300))
try(seq(Inf))
try(seq(1, Inf))
try(seq(-Inf, 3, by = 1))
try(seq(1e300))
try(seq(1, 1e300))
