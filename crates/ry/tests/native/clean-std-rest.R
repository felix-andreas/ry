# Standard library functions with native code that no other program of this corpus calls.
dict_tour <- function(words: List<Str>) -> I64 {
  counts <- Dict.empty()
  for (word in words) {
    counts <- Dict.insert(counts, word, Str.count_utf8_bytes(word))
  }
  counts <- Dict.remove(counts, "pear")
  total <- List.len(Dict.keys(counts)) + List.sum(Dict.values(counts))
  if (Dict.contains(counts, "fig")) total + 100 else total
}
list_tour <- function(xs: List<I64>) -> I64 {
  List.sum(List.drop_first(xs, 1)) * 10 + List.sum(List.take_last(xs, 2))
}
failures <- function(texts: List<Str>) -> I64 {
  errors <- List.keep_errs(texts, function(text: Str) -> Result<I64, :InvalidNumStr> Str.to_i64(text))
  List.len(errors)
}
bits <- function(x: I64) -> I64 {
  Num.bitwise_or(Num.shift_right_by(x, 2), Num.shift_right_zf_by(x, 1)) + Num.bitwise_not(x)
}
suffixed <- function(names: List<Str>) -> I64 {
  count: I64 <- 0
  for (name in names) {
    if (Str.ends_with(name, ".R")) count <- count + 1
  }
  count
}
fruit <- ["fig", "pear", "plum"]
print(dict_tour(fruit))
numbers <- [1, 2, 3, 4]
print(list_tour(numbers))
texts <- ["1", "two", "3", "four"]
print(failures(texts))
print(bits(40))
files <- ["a.R", "b.py", "c.R"]
print(suffixed(files))
