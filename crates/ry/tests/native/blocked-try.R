# blocked: add_strings
# `try` returns an error from the enclosing function. `add_matched` is the same function written
# with `match`, which compiles.
add_strings <- function(a: Str, b: Str) -> Result<I64, :InvalidNumStr> {
  x <- try Str.to_i64(a)
  y <- try Str.to_i64(b)
  :Ok(x + y)
}
add_matched <- function(a: Str, b: Str) -> Result<I64, :InvalidNumStr> {
  match (Str.to_i64(a)) {
    :Err(error) => :Err(error),
    :Ok(x) => match (Str.to_i64(b)) {
      :Err(error) => :Err(error),
      :Ok(y) => :Ok(x + y)
    }
  }
}
total <- function(a: Str, b: Str) -> I64 {
  tried <- Result.with_default(add_strings(a, b), 0)
  matched <- Result.with_default(add_matched(a, b), 0)
  tried + matched
}
print(total("4", "5"))
print(total("4", "five"))
