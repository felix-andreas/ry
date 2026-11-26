# A local function returns early natively, with `return` or with `try`: each early value joins the
# body's value, so a tag function may return several tags from several places.
classify_all <- function(xs: List<I64>) -> I64 {
  kind <- function(x: I64) -> :Small | :Big | :Zero {
    if (x == 0) return(:Zero)
    if (x > 10) return(:Big)
    :Small
  }
  first_over <- function(limit: I64) -> I64 {
    for (x in xs) {
      if (x > limit) return(x)
    }
    -1
  }
  outer <- function(x: I64) -> I64 {
    inner <- function(y: I64) -> I64 {
      if (y < 0) return(0)
      y * 2
    }
    if (x == 5) return(inner(-x))
    inner(x) + 1
  }
  score: I64 <- 0
  for (x in xs) {
    score <- score + match (kind(x)) {
      :Zero => 1,
      :Big => 100,
      :Small => 10
    }
  }
  score + first_over(10) * 1000 + first_over(99) + outer(5) + outer(3) * 10000
}
add_one <- function(a: Str) -> Result<I64, :InvalidNumStr> {
  parsed <- function(text: Str) -> Result<I64, :InvalidNumStr> {
    x <- try Str.to_i64(text)
    :Ok(x * 10)
  }
  y <- try parsed(a)
  :Ok(y + 1)
}
clamp_all <- function(low: F64, high: F64) -> F64 {
  clamp <- function(v: F64) -> F64 {
    if (v > 1) return(1)
    v
  }
  List.sum(List.map([low, high], clamp))
}
xs <- [0, 3, 12, 5]
cat(Num.to_r(classify_all(xs)), "\n")
cat(Num.to_r(Result.with_default(add_one("4"), 0)), Num.to_r(Result.with_default(add_one("four"), 0)), "\n")
cat(clamp_all(0.5, 3), "\n")
