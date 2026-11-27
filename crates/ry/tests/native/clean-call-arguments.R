# Calls of typed and local functions match named arguments to the formals as R does, by exact
# name, then by a prefix, then by position, and a left-out formal takes its default.
area <- function(width: F64, height: F64) -> F64 width * height
scaled <- function(x: F64, factor: F64 = 2.0, offset: F64 = factor / 2.0) -> F64 x * factor + offset
named <- function(side: F64) -> F64 area(height = 2.0 * side, width = side)
prefixed <- function(side: F64) -> F64 area(h = side, side + 1.0)
defaulted <- function(x: F64) -> F64 scaled(x) + scaled(x, offset = 0.0) + scaled(x, 3.0)
local_named <- function(x: F64) -> F64 {
  minus <- function(a, b = 1.0) a - b
  minus(b = x, a = 10.0) + minus(x)
}
counted <- function(n: I64, step: I64 = 1i64) -> I64 if (n <= 0i64) 0i64 else 1i64 + counted(n - step)
print(named(3.0))
print(prefixed(3.0))
print(defaulted(1.5))
print(local_named(4.0))
print(counted(10i64))
