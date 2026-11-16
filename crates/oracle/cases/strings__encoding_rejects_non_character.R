# `Encoding()` takes a character vector only. Every other type is an error, never a read of
# the vector's elements as strings.
arguments <- list(c(1.5, 2), 1:3, seq_len(70) / 4, TRUE, 1i, as.raw(1:2), list(1, "a"), NULL, quote(a), factor("a"))
for (argument in arguments) {
  cat(typeof(argument), ": ", tryCatch(Encoding(argument), error = function(e) conditionMessage(e)), "\n", sep = "")
}
Encoding(c("a", "ä", NA))
