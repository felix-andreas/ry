# expect: ok
# A literal arm result runs as the declared I64, not as an R double.
first_or_zero <- function(xs: List<I64>) -> I64 match (List.first(xs)) {
  :Ok(x) => x,
  :Err(:ListWasEmpty) => 0
}
print(first_or_zero(List.drop_first([5], 1)))
#> 0i64
