# A `match` on a tuple, and a local function that reads a name set after its definition.
first <- function(pair: Tuple{I64, Str}) -> I64 {
  match (pair) {
    (a, _) => a
  }
}
of_pair <- function(n: I64) -> I64 first((n, "a"))
shifted <- function(n: I64) -> I64 {
  add <- function(a: I64) -> I64 a + step
  step <- 2
  add(n)
}
cat(Num.to_r(of_pair(3)), Num.to_r(shifted(3)), "\n")
