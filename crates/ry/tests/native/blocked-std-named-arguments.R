# blocked: words
# A standard library call with a named argument.
words <- function(text: Str) -> I64 {
  List.len(Str.split_on(text, sep = " "))
}
cat(Num.to_r(words("a b c")), "\n")
