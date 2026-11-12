# Dictionaries, sets, strings, format strings and results.
word_counts <- function(text: Str) -> Dict<Str, I64> {
  counts: Dict<Str, I64> <- Dict.empty()
  for (word in Str.split_on(text, " ")) {
    counts <- Dict.insert(counts, word, Result.with_default(Dict.get(counts, word), 0) + 1)
  }
  counts
}
describe <- function(text: Str) -> Str {
  counts <- word_counts(Str.with_ascii_lowercased(text))
  distinct <- Set.len(Set.from_list(Str.split_on(text, " ")))
  the <- Result.with_default(Dict.get(counts, "the"), 0)
  parsed <- match (Str.to_i64("42")) {
    :Ok(number) => number
    :Err(_) => 0
  }
  f"{Dict.len(counts)} words, {distinct} spellings, {the} the, {parsed}"
}
cat(describe("The cat and the hat and THE bat"), "\n")
