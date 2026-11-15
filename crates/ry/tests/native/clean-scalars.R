# Numbers, conditions, loops, early returns and recursion.
collatz <- function(start: I64) -> I64 {
  steps: I64 <- 0
  n <- start
  while (n != 1) {
    if (n %% 2 == 0) n <- n %/% 2 else n <- 3 * n + 1
    steps <- steps + 1
  }
  steps
}
gcd <- function(a: I64, b: I64) -> I64 {
  if (b == 0) return(a)
  gcd(b, a %% b)
}
mean_of_squares <- function(n: I64) -> F64 {
  total <- 0
  for (i in List.range(1, n)) {
    x <- Num.to_f64(i)
    total <- total + x * x
  }
  total / Num.to_f64(n)
}
classify <- function(x: F64, strict: Bool) -> Str {
  if (Num.is_nan(x)) return("nan")
  label: Str <- if (x < 0) "negative" else if (x == 0) "zero" else "positive"
  if (strict && Num.abs(x) > 100) "large" else label
}
cat(Num.to_r(collatz(27)), Num.to_r(gcd(1071, 462)), mean_of_squares(10), classify(-3, TRUE), classify(250, TRUE), "\n")
