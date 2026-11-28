# Parts of a list taken with single brackets: a position, positions, left-out positions and a mask.
alias Point = Record{x: I64, y: I64}
second <- function(xs: List<I64>) -> I64 {
  part <- xs[2]
  List.len(part)
}
parts <- function(xs: List<I64>, at: List<I64>, i: I64) -> I64 {
  List.sum(xs[at]) + List.sum(xs[i]) + List.sum(xs[-1]) + List.len(xs[0])
}
points <- function(n: I64, at: List<I64>) -> I64 {
  ps <- List.map(List.range(1, n), \(i) (x = i, y = i * 10))
  kept <- ps[at]
  masked <- ps[List.map(ps, \(p) p$x > 1)]
  List.walk(kept, 0, \(total, p) total + p$y) + List.len(masked)
}
words <- function(ws: List<Str>) -> Str Str.join_with(ws[-2], "-")
xs <- [5, 6, 7]
cat(Num.to_r(second(xs)), Num.to_r(parts(xs, [3, 1], 2)), Num.to_r(points(4, [4, 4, 1])), "\n")
cat(words(["a", "b", "c"]), "\n")
