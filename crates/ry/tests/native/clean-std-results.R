# Result-callback walks and maps, splits of lists and strings, capacity hints and checked
# conversions.
parsed_total <- function(words: List<Str>) -> Result<I64, :InvalidNumStr> {
  List.walk_try(words, 0, function(total: I64, word: Str) -> Result<I64, :InvalidNumStr> {
    Result.map(Str.to_i64(word), function(n: I64) -> I64 total + n)
  })
}
parsed_all <- function(words: List<Str>) -> Result<List<I64>, :InvalidNumStr> {
  List.map_try(words, Str.to_i64)
}
large <- function(words: List<Str>) -> Result<List<Str>, :InvalidNumStr> {
  List.keep_if_try(words, function(word: Str) -> Result<Bool, :InvalidNumStr> {
    Result.map(Str.to_i64(word), function(n: I64) -> Bool n > 2)
  })
}
grown <- function(xs: List<I64>, word: Str) -> List<I64> {
  List.prepend_if_ok(List.append_if_ok(xs, Str.to_i64(word)), Str.to_i64(word))
}
blend <- function(a: List<F64>, b: List<F64>, c: List<F64>, d: List<F64>) -> List<F64> {
  List.map4(a, b, c, d, function(w: F64, x: F64, y: F64, z: F64) -> F64 w * x + y * z)
}
pieces <- function(xs: List<I64>) -> I64 {
  List.len(List.split_on(xs, 0)) * 10 + List.len(List.split_on_list(xs, [0, 0]))
}
doubled <- function(words: List<Str>) -> I64 {
  counts <- Dict.empty()
  for (word in words) {
    counts <- Dict.insert(counts, word, Str.count_utf8_bytes(word))
  }
  both <- Dict.join_map(Dict.reserve(counts, 4), function(word: Str, n: I64) -> Dict<Str, I64> {
    Dict.insert(Dict.single(word, n), Str.concat(word, word), n * 2)
  })
  emptied <- Dict.release_excess_capacity(Dict.clear(both))
  Dict.len(both) * 10 + Dict.len(emptied)
}
roomy <- function(xs: List<I64>) -> I64 List.len(List.release_excess_capacity(List.reserve(xs, 10)))
whole <- function(x: F64) -> I64 Result.with_default(Num.to_i64_checked(x), -1)
around <- function(text: Str) -> Str {
  key <- match (Str.split_first(text, "=")) {
    :Ok(parts) => parts$before
    :Err(_) => "none"
  }
  rest <- match (Str.split_last(text, "=")) {
    :Ok(parts) => parts$after
    :Err(_) => "none"
  }
  f"{key} {rest} {Str.replace_first(text, "=", ":")} {Str.replace_last(text, "=", ":")}"
}
words <- ["1", "2", "3"]
print(parsed_total(words))
print(parsed_all(words))
print(large(words))
ones <- [1]
print(grown(ones, "2"))
floats <- [1.5, 2.5]
print(blend(floats, floats, floats, floats))
numbers <- [1, 0, 2, 0, 0, 3]
print(pieces(numbers))
print(doubled(words))
print(roomy(numbers))
print(whole(4))
print(whole(4.5))
print(around("key=value=more"))
