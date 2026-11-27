# A function that R returns invisibly runs natively, and its call shows the value as R does: a
# loop, an assignment or `List.for_each` at the end, an `if` without `else` around a loop, a path
# that stops, and a call that ends in a local, typed or passed function that gives its value
# invisibly.
show_all <- function(n: I64) -> NULL {
  for (i in List.range(1, n)) cat("row", Num.to_r(i), "\n")
}
remember <- function(x: F64) -> F64 {
  doubled <- x * 2
}
bump_all <- function(xs: List<I64>) -> NULL List.for_each(xs, function(x: I64) {
  cat(Num.to_r(x + 1), "\n")
  NULL
})
maybe_show <- function(n: I64) -> NULL {
  if (n > 0) for (i in List.range(1, n)) cat("maybe", Num.to_r(i), "\n")
}
guarded <- function(x: F64) -> F64 {
  if (x < 0) stop("negative")
  y <- x + 1
}
through_local <- function(x: F64) -> F64 {
  keep <- function(y: F64) -> F64 { z <- y + 1 }
  keep(x)
}
through_typed <- function(n: I64) -> NULL show_all(n)
shown <- function(x: F64) -> F64 {
  y <- x * 3
  y
}
show_all(2)
remember(2)
print(remember(2))
bump_all([1, 2])
maybe_show(1)
maybe_show(0)
guarded(1)
through_local(1)
through_typed(1)
shown(1)
cat(remember(1), guarded(2), through_local(2), "\n")

apply_quiet <- function(f: fn(F64) -> F64, x: F64) -> F64 f(x)
passed <- function(x: F64) -> F64 apply_quiet(\(y) { z <- y }, x)
passed(5)
print(passed(5))
quiet <- function(y: F64) -> F64 { z <- y }
apply_local <- function(f: fn(F64) -> F64, x: F64) -> F64 {
  g <- function(y: F64) -> F64 f(y)
  g(x)
}
via_local <- function(x: F64) -> F64 apply_local(quiet, x)
local_pass <- function(x: F64) -> F64 {
  g <- function(h: fn(F64) -> F64, y: F64) -> F64 h(y)
  g(\(v) { w <- v }, x)
}
twice <- function(f: fn(F64) -> F64, x: F64) -> F64 f(x)
nested <- function(x: F64) -> F64 twice(\(y) twice(quiet, y), x)
loud <- function(x: F64) -> F64 twice(\(y) y + 1, x)
counted <- function(n: I64, f: fn(F64) -> F64) -> F64 if (n == 0) f(1) else counted(n - 1, f)
through_recursion <- function(x: F64) -> F64 counted(3, quiet)
via_local(5)
local_pass(6)
nested(3)
loud(3)
through_recursion(1)

# A loop that only a `return` leaves gives the returned value, visibly.
first_above <- function(x: F64) -> F64 {
  repeat {
    x <- x + 1
    if (x > 3) return(x)
  }
}
newton <- function(x: F64) -> F64 {
  guess <- x
  while (TRUE) {
    better <- (guess + x / guess) / 2
    if (abs(better - guess) < 1e-9) return(better)
    guess <- better
  }
}
count_to <- function(n: I64) -> NULL {
  i <- 0i64
  repeat {
    i <- i + 1i64
    if (i > n) break
  }
}
first_above(1)
newton(2)
count_to(3)
print(count_to(3))
