# A colon sequence with an infinite or huge bound is an error, not an allocation.
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
