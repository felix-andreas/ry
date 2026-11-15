# blocked: show_dict, show_set, show_list, show_number
# A ry value R does not take as it is.
show_dict <- function(n: I64) -> I64 {
  counts <- Dict.insert(Dict.empty(), "a", n)
  print(counts)
  n
}
show_set <- function(xs: List<I64>) -> I64 {
  seen <- Set.from_list(xs)
  print(seen)
  List.len(xs)
}
show_list <- function(xs: List<I64>) -> I64 {
  print(xs)
  List.len(xs)
}
show_number <- function(n: I64) -> I64 {
  print(n)
  n
}
converted <- function(n: I64, xs: List<I64>) -> I64 {
  print(Num.to_r(n))
  print(List.to_r(xs))
  n
}
of_range <- function(n: I64) -> I64 converted(n, List.range(1, 3))
xs <- [1, 2, 3]
cat(Num.to_r(show_dict(2)), Num.to_r(show_set(xs)), Num.to_r(show_list(xs)), "\n")
cat(Num.to_r(show_number(2)), Num.to_r(of_range(2)), "\n")
