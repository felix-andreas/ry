# blocked: parsed
parsed <- function(first: Str) -> I64 {
  numbers <- List.map_try([first, "2", "3"], Str.to_i64)
  Result.with_default(Result.map(numbers, List.sum), 0)
}
print(parsed("1"))
