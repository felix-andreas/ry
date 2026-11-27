# Recursive functions that take a function: the function swapped between arguments, a typed
# function by name, lambdas that read a caller's parameter, a global or a loop variable, tree and
# mutual recursion, and a recursion deeper than an inlined one could be.
swap <- function(n: I64, f: fn(k: I64) -> I64, g: fn(k: I64) -> I64) -> I64 {
  if (n == 0i64) 0i64 else f(n) - g(n) + swap(n - 1i64, g, f)
}
both <- function(n: I64) -> I64 swap(n, \(k) k * 2i64, \(k) k + 1i64)
print(both(5i64))
triple <- function(k: I64) -> I64 k * 3i64
count_down <- function(n: I64, f: fn(k: I64) -> I64) -> I64 {
  if (n == 0i64) 0i64 else f(n) + count_down(n - 1i64, f)
}
named <- function(n: I64) -> I64 count_down(n, triple)
print(named(4i64))
m <- 100i64
shifted <- function(n: I64, m: I64) -> I64 count_down(n, \(k) k + m)
print(shifted(3i64, 1i64))
global <- function(n: I64) -> I64 count_down(n, \(k) k + m)
print(global(3i64))
tree <- function(n: I64, f: fn(k: I64) -> I64) -> I64 {
  if (n <= 1i64) f(n) else tree(n - 1i64, f) + tree(n - 2i64, f)
}
fib_squares <- function(n: I64) -> I64 tree(n, \(k) k * k)
print(fib_squares(20i64))
ping <- function(n: I64, f: fn(k: I64) -> I64) -> I64 if (n == 0i64) 0i64 else f(n) + pong(n - 1i64, f)
pong <- function(n: I64, f: fn(k: I64) -> I64) -> I64 if (n == 0i64) 0i64 else 2i64 * f(n) + ping(n - 1i64, f)
ping_pong <- function(n: I64, by: I64) -> I64 ping(n, \(k) k * by)
print(ping_pong(6i64, 10i64))
deep <- function(n: I64) -> I64 count_down(n, \(k) 1i64)
print(deep(2000i64))
looped <- function(n: I64) -> I64 {
  total <- 0i64
  for (i in List.range(1i64, n)) {
    total <- total + count_down(3i64, \(k) k * i)
  }
  total
}
print(looped(4i64))
fold <- function(n: I64, f: fn(k: I64) -> I64) -> I64 if (n <= 0i64) 0i64 else f(n) + fold(n - 1i64, f)
through_local <- function(n: I64) -> I64 {
  square <- function(x: I64) -> I64 x * x
  fold(n, \(k) square(k) + 1i64)
}
print(through_local(3i64))
on_some_paths <- function(n: I64, flag: Bool) -> I64 {
  if (flag) by <- 2i64
  fold(n, \(k) k * by)
}
print(on_some_paths(3i64, TRUE))
with_helper <- function(n: I64, f: fn(k: I64) -> I64) -> I64 {
  helper <- function(x: I64) -> I64 x + 1i64
  if (n <= 0i64) 0i64 else f(n) + with_helper(n - 1i64, \(k) helper(k))
}
started <- function(n: I64) -> I64 with_helper(n, \(k) k * 10i64)
print(started(3i64))
wrapping <- function(n: I64, g: fn(k: I64) -> I64) -> I64 fold(n, \(k) g(k) * 2i64)
wrapped <- function(n: I64) -> I64 wrapping(n, \(k) k + 5i64)
print(wrapped(3i64))
