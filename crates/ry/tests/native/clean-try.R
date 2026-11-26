# `try` runs natively: it gives an `:Ok`'s payload, and on an `:Err` it returns that `:Err` from the
# function, inside a loop, inside an expression and in a recursive function alike.
parse_all <- function(words: List<Str>) -> Result<I64, :InvalidNumStr> {
  total <- 0
  for (word in words) {
    total <- total + try Str.to_i64(word)
  }
  :Ok(total)
}
halve <- function(n: I64) -> Result<I64, Str> {
  if (Num.is_odd(n)) :Err("odd") else :Ok(Num.div_trunc(n, 2))
}
ratio <- function(n: I64) -> Result<F64, Str> {
  half <- try halve(n)
  quarter <- try halve(half)
  :Ok(Num.to_f64(quarter) / 3.0)
}
both <- function(a: I64, b: I64) -> Result<I64, Str> {
  :Ok(try halve(a) + try halve(b))
}
countdown <- function(n: I64) -> Result<I64, Str> {
  if (n == 0) return(:Ok(0))
  rest <- try countdown(n - 1)
  step <- try halve(n * 2)
  :Ok(rest + step)
}
scaled <- function(n: I64) -> F64 {
  Result.with_default(ratio(n), 0.0) * 2.0
}
again <- function(n: I64) -> Result<F64, Str> {
  r <- try ratio(n)
  :Ok(r + 1.0)
}
report <- function(a: I64, b: I64) -> Str {
  quarter <- match (ratio(a)) {
    :Ok(x) => f"{x}",
    :Err(e) => e
  }
  sum <- Result.with_default(both(a, b), -1)
  steps <- Result.with_default(countdown(b), -1)
  twice <- scaled(a) + Result.with_default(again(a), 0.0)
  f"{quarter} {sum} {steps} {twice}"
}
words <- ["1", "2", "30"]
bad <- ["1", "x", "30"]
cat(Num.to_r(Result.with_default(parse_all(words), -1)), Num.to_r(Result.with_default(parse_all(bad), -1)), "\n")
first <- 8
second <- 6
third <- 3
cat(report(first, second), "\n")
cat(report(third, second), "\n")
cat(report(second, third), "\n")
