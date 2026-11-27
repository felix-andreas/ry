# blocked: groups
groups <- function(first: Str, second: Str) -> I64 {
  seen: Dict<Str, Set<Str>> <- Dict.empty()
  for (name in [first, second, first]) seen <- Dict.insert(seen, name, Set.single(name))
  Dict.len(seen)
}
cat(Num.to_r(groups("a", "b")), "\n")
