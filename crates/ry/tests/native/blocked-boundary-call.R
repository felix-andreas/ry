# blocked: total_length
# boundary: text_length
# A project function with an R type in its signature counts as an R function.
text_length <- function(text: character) -> F64 Num.to_f64(Num.from_r(nchar(text)))
total_length <- function(first: Str, second: Str) -> F64 text_length(first) + text_length(second)
cat(total_length("ab", "cde"), "\n")
