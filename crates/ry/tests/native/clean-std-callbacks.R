# Result and list functions that take a callback, list updates, and the overflow-aware and
# string functions.
flag <- function(b: Bool) -> I64 {
  if (b) 1 else 0
}
halve <- function(n: I64) -> Result<I64, Str> {
  if (Num.is_odd(n)) :Err("odd") else :Ok(Num.div_trunc(n, 2))
}
results <- function(n: I64) -> I64 {
  a <- Result.with_default(Result.map(halve(n), \(x: I64) -> I64 x + 1), -1)
  b <- Result.with_default(Result.and_then(halve(n), halve), -1)
  c <- Result.with_default(Result.map_err(halve(n), \(e: Str) -> I64 Str.count_utf8_bytes(e)), -1)
  d <- Result.with_default(Result.on_err(halve(n), \(e: Str) -> Result<I64, Str> :Ok(0)), -1)
  e <- Result.with_default(Result.map_both(halve(n), \(x: I64) -> I64 x * 10, \(e: Str) -> Str e), -1)
  a + b + c + d + e
}
lists <- function(xs: List<I64>) -> List<I64> {
  [
    flag(List.any(xs, \(x: I64) -> Bool x > 3)),
    flag(List.all(xs, \(x: I64) -> Bool x > 0)),
    List.len(List.drop_if(xs, \(x: I64) -> Bool Num.is_even(x))),
    List.product(xs),
    Result.with_default(List.first(List.prepend(xs, 7)), 0),
    List.len(List.single(3)),
    List.sum(List.set(xs, 2, 100)),
    List.len(List.set(xs, 0, 5)),
    List.len(List.drop_at(xs, 1)),
    Result.with_default(List.find_last(xs, \(x: I64) -> Bool x < 3), 0),
    Result.with_default(List.find_last_index(xs, \(x: I64) -> Bool x < 3), 0),
    List.walk_until(xs, 0, \(s: I64, x: I64) -> :Continue(I64) | :Break(I64) if (s > 5) :Break(s) else :Continue(s + x)),
    List.walk_until(xs, 0, \(s: I64, x: I64) -> :Continue(I64) | :Break(I64) :Break(s + x)),
    List.walk_backwards_until(xs, 0, \(s: I64, x: I64) -> :Continue(I64) | :Break(I64) if (x == 2) :Break(s) else :Continue(s + x)),
    List.walk_with_index(xs, 0, \(s: I64, x: I64, i: I64) -> I64 s + x * i),
    List.walk_with_index_until(xs, 0, \(s: I64, x: I64, i: I64) -> :Continue(I64) | :Break(I64) if (i > 2) :Break(s) else :Continue(s + x * i)),
    List.walk_backwards(xs, 0, \(s: I64, x: I64) -> I64 s * 2 + x)
  ]
}
strings <- function(s: Str) -> List<Str> {
  [
    Str.trim(s), Str.trim_start(s), Str.trim_end(s), Str.drop_prefix(s, " "),
    Str.drop_suffix(s, "é "), Str.with_prefix(s, "→"),
    Num.to_str(flag(Str.contains(s, "x"))), Num.to_str(flag(Str.caseless_ascii_equals(s, " AB ")))
  ]
}
numbers <- function(near: I64) -> List<I64> {
  top <- Num.sub(Num.max_i64, near)
  low <- Num.add(Num.min_i64, near)
  [
    Num.add_wrap(top, 1000), Num.sub_wrap(low, 1000), Num.mul_wrap(top, 2),
    Num.add_saturated(top, 1000), Num.sub_saturated(low, 1000), Num.mul_saturated(low, 3),
    Result.with_default(Num.add_checked(top, 1000), 0), Result.with_default(Num.sub_checked(17, 4), 0),
    Result.with_default(Num.mul_checked(top, 2), 0),
    Num.div_ceil(17, 4), Num.div_ceil(-17, 4), Num.div_ceil(-17, -4), Num.div_ceil(16, 4),
    Num.count_one_bits(top), Num.count_leading_zero_bits(near), Num.count_trailing_zero_bits(-8)
  ]
}
dicts <- function(k: Str) -> I64 {
  d <- Dict.single(k, 1)
  flag(Dict.is_empty(d)) + Dict.len(d)
}
print(results(8))
print(results(7))
print(lists([1, 2, 3, 4]))
print(strings("  xé "))
print(numbers(5))
print(dicts("a"))
