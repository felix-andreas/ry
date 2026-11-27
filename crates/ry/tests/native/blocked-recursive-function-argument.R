# blocked: nest
# A function literal passed to a recursive call that calls the caller's function parameter.
nest <- function(n: I64, f: fn(k: I64) -> I64) -> I64 if (n == 0i64) f(0i64) else nest(n - 1i64, \(k) f(k) + 1i64)
top <- function(n: I64) -> I64 nest(n, \(k) k)
print(top(3i64))
