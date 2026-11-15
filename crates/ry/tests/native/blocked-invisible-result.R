# blocked: remember, show_all
remember <- function(x: F64) -> F64 {
  doubled <- x * 2
}
show_all <- function(n: I64) -> NULL {
  for (i in List.range(1, n)) {
    cat("row", Num.to_r(i), "\n")
    NULL
  }
}
show_all(2)
cat(remember(2), "\n")
