# blocked: distinct
distinct <- function(first: Str, second: Str) -> I64 {
  seen: Dict<Str, Bool> <- Dict.empty()
  for (name in [first, second, first]) seen <- Dict.insert(seen, name, TRUE)
  Dict.len(seen)
}
cat(Num.to_r(distinct("a", "b")), "\n")
