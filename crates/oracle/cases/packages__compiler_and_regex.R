# Base R gaps that broke package loading: format() of an environment, the compiler package's
# internals behind cmpfun, readChar's three arguments, and TRE's non-capturing groups.
environment_text <- format(new.env())
print(startsWith(environment_text, "<environment: "))
print(format(globalenv()))
print(length(format(list(new.env(), 1:2))))
print(.Internal(bcVersion()))
compiled <- compiler::cmpfun(function(x, y = 2) {
  z <- x * y
  if (z > 3) "big" else "small"
})
print(c(compiled(1), compiled(5)))
print(body(compiled))
path <- tempfile()
writeLines("hello world", path)
print(readChar(path, c(5, 1, 5)))
print(gsub("a(?:b|c)", "X", "abac"))
print(sub("(?:a)(b)", "<\\1>", "zab"))
print(grepl("(?i)ABC", "xabc"))
print(gsub("\\\\([0-9a-f]{1,6})(?:\r\n|[ \n\r\t\f])?", "\\1", "x\\41 y", ignore.case = TRUE))
