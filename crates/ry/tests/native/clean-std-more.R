# Standard library functions with native code: walks from a position and over dictionaries,
# list splits and edits, dictionary filters and merges, `Result.map2` and checked numbers.
labels <- function(xs: List<Str>) -> List<Str> {
  List.map_with_index(xs, function(x: Str, i: I64) -> Str Str.concat(x, Num.to_str(i)))
}
tail_sum <- function(xs: List<I64>) -> I64 {
  List.walk_from(xs, 2, 0, function(s: I64, x: I64) -> I64 s + x)
}
tail_until <- function(xs: List<I64>) -> I64 {
  List.walk_from_until(xs, 2, 0, function(s: I64, x: I64) if (x > 5) :Break(s) else :Continue(s + x))
}
counts <- function(words: List<Str>) -> Dict<Str, I64> {
  Dict.from_list(List.map(words, function(w: Str) -> Tuple{Str, I64} (w, Str.count_utf8_bytes(w))))
}
total_values <- function(d: Dict<Str, I64>) -> I64 {
  Dict.walk(d, 0, function(s: I64, k: Str, v: I64) -> I64 s + v)
}
values_until <- function(d: Dict<Str, I64>) -> I64 {
  Dict.walk_until(d, 0, function(s: I64, k: Str, v: I64) if (v > 2) :Break(s) else :Continue(s + v))
}
scaled <- function(d: Dict<Str, I64>) -> Dict<Str, I64> {
  Dict.map(d, function(k: Str, v: I64) -> I64 v * 10)
}
big <- function(d: Dict<Str, I64>) -> Dict<Str, I64> {
  Dict.keep_if(d, function(k: Str, v: I64) -> Bool v > 1)
}
small <- function(d: Dict<Str, I64>) -> Dict<Str, I64> {
  Dict.drop_if(d, function(k: Str, v: I64) -> Bool v > 1)
}
merged <- function(a: Dict<Str, I64>, b: Dict<Str, I64>) -> Dict<Str, I64> {
  Dict.insert_all(a, b)
}
shared <- function(a: Dict<Str, I64>, b: Dict<Str, I64>) -> Dict<Str, I64> {
  Dict.keep_shared(a, b)
}
without <- function(a: Dict<Str, I64>, b: Dict<Str, I64>) -> Dict<Str, I64> {
  Dict.remove_all(a, b)
}
halves <- function(xs: List<I64>) -> Tuple{List<I64>, List<I64>} {
  parts <- List.split_at(xs, 2)
  (parts$before, parts$others)
}
around <- function(xs: List<I64>) -> I64 {
  match (List.split_first(xs, 0)) {
    :Ok(parts) => List.len(parts$before) * 10 + List.len(parts$after)
    :Err(_) => -1
  }
}
around_last <- function(xs: List<I64>) -> I64 {
  match (List.split_last(xs, 0)) {
    :Ok(parts) => List.len(parts$before) * 10 + List.len(parts$after)
    :Err(_) => -1
  }
}
prefixed <- function(xs: List<Str>) -> Bool {
  List.starts_with(xs, ["a", "b"])
}
suffixed <- function(xs: List<Str>) -> Bool {
  List.ends_with(xs, ["b", "c"])
}
spaced <- function(xs: List<I64>) -> List<I64> {
  List.intersperse(xs, 0)
}
flat <- function(xs: List<List<I64>>) -> List<I64> {
  List.join(xs)
}
flat_tour <- function(xs: List<I64>) -> List<I64> {
  flat([[1], List.drop_first(xs, 2), xs])
}
replaced <- function(xs: List<I64>) -> I64 {
  r <- List.replace(xs, 2, 7)
  List.sum(r$list) * 100 + r$value
}
bumped <- function(xs: List<I64>) -> List<I64> {
  List.update(xs, 2, function(x: I64) -> I64 x + 1)
}
each <- function(xs: List<I64>) -> I64 {
  List.for_each(xs, function(x: I64) x + 1)
  List.len(xs)
}
both <- function(a: Result<I64, Str>, b: Result<I64, Str>) -> Result<I64, Str> {
  Result.map2(a, b, function(x: I64, y: I64) -> I64 x + y)
}
quotient <- function(a: F64, b: F64) -> Result<F64, :DivByZero | :Undefined> {
  Num.div_checked(a, b)
}
truncated <- function(a: I64, b: I64) -> Result<I64, :DivByZero | :Undefined> {
  Num.div_trunc_checked(a, b)
}
ceiled <- function(a: I64, b: I64) -> Result<I64, :DivByZero | :Undefined> {
  Num.div_ceil_checked(a, b)
}
remainder <- function(a: I64, b: I64) -> Result<I64, :DivByZero | :Undefined> {
  Num.rem_checked(a, b)
}
root <- function(a: F64) -> Result<F64, :SqrtOfNegative> {
  Num.sqrt_checked(a)
}
logged <- function(a: F64) -> Result<F64, :LogNeedsPositive> {
  Num.log_checked(a)
}
close <- function(a: F64, b: F64) -> Bool {
  Num.is_approx_eq(a, b) && !Num.is_approx_eq(a, b + 1.0, atol = 0.5) && Num.is_approx_eq(atol = 0.5, a, b + 0.25)
}
others <- function() -> Dict<Str, I64> {
  Dict.from_list([("bb", 2), ("ccc", 0), ("z", 5)])
}
print(labels(["a", "b"]))
print(tail_sum([1, 2, 3]))
print(tail_until([1, 2, 3, 9, 4]))
dict_tour <- function(words: List<Str>) -> List<Str> {
  d <- counts(words)
  e <- others()
  shown <- function(dict: Dict<Str, I64>) -> Str {
    start: Str <- ""
    Dict.walk(dict, start, function(text: Str, k: Str, v: I64) -> Str Str.concat(text, Str.concat(k, Num.to_str(v))))
  }
  [
    Num.to_str(total_values(d)), Num.to_str(values_until(d)), shown(d), shown(scaled(d)),
    shown(big(d)), shown(small(d)), shown(merged(d, e)), shown(shared(d, e)), shown(without(d, e))
  ]
}
words <- ["a", "bb", "a", "ccc"]
print(dict_tour(words))
print(halves([1, 2, 3]))
print(around([1, 0, 2, 0, 3, 4]))
print(around_last([1, 0, 2, 0, 3, 4]))
print(around([1, 2]))
print(prefixed(["a", "b", "c"]))
print(suffixed(["a", "b", "c"]))
print(prefixed(["a"]))
print(spaced([1, 2, 3]))
print(flat_tour([2, 3]))
print(replaced([1, 2, 3]))
print(replaced([1]))
print(bumped([1, 2, 3]))
print(each([1, 2]))
print(both(:Ok(1), :Ok(2)))
print(both(:Err("x"), :Ok(2)))
print(both(:Ok(1), :Err("y")))
print(quotient(7.0, 2.0))
print(quotient(7.0, 0.0))
print(truncated(-7, 2))
print(truncated(-7, 0))
print(ceiled(7, 2))
print(remainder(-7, 2))
print(root(4.0))
print(root(-1.0))
print(logged(1.0))
print(logged(0.0))
print(close(0.1 + 0.2, 0.3))
