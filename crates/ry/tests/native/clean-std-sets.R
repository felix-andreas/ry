# The Set family: building, membership, algebra, walks, callbacks and capacity.
distinct <- function(xs: List<I64>) -> I64 Set.len(Set.from_list(xs))
present <- function(xs: List<Str>, x: Str) -> Bool Set.contains(Set.from_list(xs), x)
grown <- function(xs: List<I64>) -> List<I64> {
  s <- Set.empty()
  for (x in xs) s <- Set.insert(s, x * 2i64)
  Set.to_list(Set.remove(s, 4i64))
}
algebra <- function(xs: List<I64>, ys: List<I64>) -> List<I64> {
  a <- Set.from_list(xs)
  b <- Set.from_list(ys)
  List.concat(List.concat(Set.to_list(Set.union(a, b)), Set.to_list(Set.intersection(a, b))), Set.to_list(Set.difference(a, b)))
}
folded <- function(xs: List<I64>) -> I64 Set.walk(Set.from_list(xs), 0i64, \(total, x) total + x)
until <- function(xs: List<I64>) -> I64 Set.walk_until(Set.from_list(xs), 0i64, \(total, x) if (total > 5i64) :Break(total) else :Continue(total + x))
filtered <- function(xs: List<I64>) -> List<I64> {
  s <- Set.from_list(xs)
  List.concat(Set.to_list(Set.keep_if(s, \(x) x > 2i64)), Set.to_list(Set.drop_if(s, \(x) x > 2i64)))
}
mapped <- function(xs: List<I64>) -> List<Str> Set.to_list(Set.map(Set.from_list(xs), \(x) Num.to_str(x %% 2i64)))
joined <- function(xs: List<I64>) -> List<I64> Set.to_list(Set.join_map(Set.from_list(xs), \(x) Set.from_list([x, x + 10i64])))
single <- function(x: Str) -> Bool Set.is_empty(Set.remove(Set.single(x), x))
roomy <- function(xs: List<I64>) -> I64 Set.len(Set.release_excess_capacity(Set.reserve(Set.from_list(xs), 8i64)))
sized <- function(xs: List<I64>) -> I64 Set.len(List.walk(xs, Set.with_capacity(4i64), \(set, x) Set.insert(set, x)))
xs <- [1i64, 2i64, 3i64, 2i64, 1i64, 5i64]
ys <- [3i64, 4i64, 5i64]
names <- ["a", "b"]
print(distinct(xs))
print(present(names, "b"))
print(grown(xs))
print(algebra(xs, ys))
print(folded(xs))
print(until(xs))
print(filtered(xs))
print(mapped(xs))
print(joined(xs))
print(single("a"))
print(roomy(xs))
print(sized(xs))
