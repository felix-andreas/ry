# scan()'s "expected" error and the invalid multibyte error are formatted into a buffer of
# warning.length bytes, so a long field loses the message's tail, never half a character.
f <- tempfile()
attempt <- function(what) {
  message <- tryCatch(scan(f, what = what, quiet = TRUE), error = conditionMessage)
  print(c(nchar(message, "bytes"), substring(message, nchar(message) - 4)))
}
for (n in c(900, 964, 965, 5000)) {
  writeLines(strrep("a", n), f)
  attempt(logical())
  attempt(integer())
}
writeLines(strrep("é", 3000), f)
attempt(integer())
writeLines(paste0("\xe9", strrep("b", 2000)), f)
attempt(double())
options(warning.length = 200)
attempt(double())
writeLines(strrep("a", 300), f)
attempt(logical())
unlink(f)
