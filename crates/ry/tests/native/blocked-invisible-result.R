# blocked: remember, show_all, bump_all
remember <- function(x: F64) -> F64 {
  doubled <- x * 2
}
show_all <- function(n: I64) -> NULL {
  for (i in List.range(1, n)) {
    cat("row", Num.to_r(i), "\n")
    NULL
  }
}
bump_all <- function(xs: List<I64>) List.for_each(xs, function(x: I64) x + 1)
show_all(2)
cat(remember(2), "\n")
bump_all([1, 2])
