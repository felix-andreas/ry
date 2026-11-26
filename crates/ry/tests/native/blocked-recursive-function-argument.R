# blocked: count_down
count_down <- function(n: I64, f: fn(I64) -> I64) -> I64 if (n <= 0) 0 else f(n) + count_down(n - 1, f)
cat(Num.to_r(count_down(3, \(k: I64) -> I64 k * 2)), "\n")
