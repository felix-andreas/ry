# Tag unions that widen through a walk's state, a loop's accumulator, the branches of an `if` and
# the list functions that store an item.
first_some <- function(xs: List<I64>) -> :None | :Some(I64) {
  List.walk(xs, :None, \(acc, x) :Some(x))
}
grow <- function(xs: List<I64>) -> List<:A | :B> {
  List.walk(xs, [:A], \(acc, x) List.append(acc, :B))
}
best <- function(xs: List<I64>) -> Record{best: :None | :Some(I64), n: I64} {
  List.walk(xs, (best = :None, n = 0i64), \(acc, x) (best = :Some(x), n = acc$n + 1i64))
}
cycle <- function(xs: List<I64>) -> :A | :B | :C {
  List.walk(xs, :A, \(acc, x) match (acc) { :A => :B; :B => :C; :C => :A })
}
loop_grow <- function(xs: List<I64>) -> I64 {
  acc <- [:A]
  for (x in xs) acc <- List.append(acc, :B)
  List.len(acc)
}
record_branch <- function(c: Bool) -> I64 {
  r <- if (c) (t = :A, n = 1i64) else (t = :B, n = 2i64)
  match (r$t) { :A => r$n; :B => r$n * 10i64 }
}
tuple_branch <- function(c: Bool) -> Tuple{:A | :B, I64} {
  if (c) (:A, 1i64) else (:B, 2i64)
}
stop_at <- function(xs: List<I64>) -> :None | :Some(I64) {
  List.walk_until(xs, :None, \(acc, x) if (x > 2i64) :Break(:Some(x)) else :Continue(acc))
}
branch <- function(c: Bool) -> List<:A | :B> {
  if (c) [:A] else [:B]
}
payload_branch <- function(c: Bool) -> :Some(List<:A | :B>) {
  if (c) :Some([:A]) else :Some([:B])
}
front <- function(xs: List<:A>) -> List<:A | :B> {
  List.prepend(xs, :B)
}
replace_first <- function(xs: List<:A>) -> List<:A | :B> {
  List.set(xs, 1i64, :B)
}
print(first_some([1i64, 2i64]))
print(grow([1i64, 2i64]))
print(best([4i64, 5i64]))
print(cycle([1i64, 2i64, 3i64, 4i64]))
print(loop_grow([1i64, 2i64, 3i64]))
print(record_branch(FALSE))
print(tuple_branch(TRUE))
print(branch(FALSE))
print(payload_branch(TRUE))
print(front([:A]))
print(replace_first([:A, :A]))
print(stop_at([1i64, 3i64]))
