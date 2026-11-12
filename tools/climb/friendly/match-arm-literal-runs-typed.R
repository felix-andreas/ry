# expect: ok
# A literal arm result runs as the declared I64, not as an R double.
parsed_or_zero <- function(text: Str) -> I64 match (Str.to_i64(text)) {
  :Ok(x) => x,
  :Err(_) => 0
}
print(parsed_or_zero("five"))
#> 0i64
