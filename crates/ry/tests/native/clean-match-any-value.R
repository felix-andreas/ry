# A `match` whose patterns only bind, on a value of any type.
size <- function(counts: Dict<Str, I64>) -> I64 {
  match (counts) {
    other => Dict.len(other)
  }
}
main <- function(key: Str) -> I64 {
  empty: Dict<Str, I64> <- Dict.empty()
  size(Dict.insert(empty, key, 1i64))
}
cat(Num.to_r(main("a")), "\n")
