# save() and load() for every compression, readChar() leaving the rest of a connection
# unread, rowsum(), and memCompress() round trips. The compressed bytes themselves differ
# between deflate implementations, so only round trips are compared.
a <- 1:3
b <- list(x = "y", m = matrix(1:4, 2), f = function(z) z + 1)
path <- tempfile(fileext = ".rda")
for (compress in list(TRUE, FALSE, "bzip2", "xz")) {
  save(a, b, file = path, compress = compress)
  e <- new.env()
  print(load(path, envir = e))
  print(identical(e$a, a))
  print(e$b$m)
  print(e$b$f(1))
}
save(a, file = path, ascii = TRUE)
e <- new.env()
print(load(path, envir = e))
f <- function() {
  local_value <- "inner"
  save(local_value, file = path)
}
f()
print(load(path))
print(local_value)
con <- gzfile(path)
print(readChar(con, 5L, useBytes = TRUE))
x <- matrix(c(1L, 2L, NA, 4L), 2, dimnames = list(NULL, c("p", "q")))
print(rowsum(x, c("b", "a")))
print(rowsum(x, c("b", "a"), na.rm = TRUE))
print(rowsum(c(1.5, 2, 3), c(2, 1, 2), na.rm = TRUE))
print(rowsum(c(1.5, 2, 3), c(2, 1, 2), reorder = FALSE))
print(rowsum(data.frame(a = 1:3, b = c(1.5, NA, 2)), c("x", "y", "x")))
print(rowsum(c(.Machine$integer.max, 1L), c(1, 1)))
text <- strrep("hello ", 20)
print(memDecompress(memCompress(text, "gzip"), "gzip", asChar = TRUE) == text)
print(memDecompress(memCompress(charToRaw(text), "xz"), "unknown", asChar = TRUE) == text)
print(memCompress(charToRaw("ab"), "none"))
print(identical(unserialize(serialize(b[1:2], NULL, ascii = TRUE)), b[1:2]))
