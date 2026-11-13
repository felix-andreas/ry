# A filter over a million doubles, counting the items above one half. The typed program holds the
# values in a List<F64>, which has no NA and no attributes, and counts them with `List.count_if`,
# as the Rust program it is measured against counts with `filter` and `count`.
array_filter <- function(x: List<F64>, times: I64) -> F64 {
  total: F64 <- 0
  for (k in List.range(1, times)) {
    total <- total + Num.to_f64(List.count_if(x, function(item: F64) -> Bool item > 0.5))
  }
  total
}
x <- List.from_r(seq(0, 1, length.out = 1000000L))
measure("array_filter", function() array_filter(x, 5))
