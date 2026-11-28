# A function whose last step calls itself runs as a loop, so its depth is not bounded by the
# stack. The parameters may hold counted values (`Str`, `List`): each round hands the next round
# its arguments, and the caller's values stay unchanged.
down <- function(n: I64, total: I64) -> I64 if (n == 0i64) total else down(n - 1i64, total + n)
build <- function(n: I64, items: List<I64>) -> List<I64> {
  if (n == 0i64) return(items)
  build(n - 1i64, List.append(items, n))
}
spell <- function(n: I64, text: Str) -> I64 {
  if (n == 0i64) Str.count_utf8_bytes(text)
  else spell(n - 1i64, if (n %% 2i64 == 0i64) text else Str.concat(text, "a"))
}
swap <- function(n: I64, a: List<I64>, b: List<I64>) -> I64 {
  if (n == 0i64) List.len(a) * 10i64 + List.len(b) else swap(n - 1i64, b, a)
}
start <- List.append(List.with_capacity(1i64), 0i64)
pair <- List.append(start, 1i64)
cat(
  Num.to_r(down(1000000i64, 0i64)),
  Num.to_r(List.len(build(1000i64, start))),
  Num.to_r(spell(1001i64, "")),
  Num.to_r(swap(1001i64, start, pair)),
  Num.to_r(List.len(start)),
  "\n"
)
