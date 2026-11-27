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
