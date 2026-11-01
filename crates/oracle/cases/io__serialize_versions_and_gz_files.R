# serialize honours the requested format version.
x <- list(a = c(1L, 5L), b = "z")
length(serialize(x, NULL, version = 2))
length(serialize(x, NULL, version = 3))
# Bytes 7 to 10 hold the writing R version, so only the format version is shown.
serialize(x, NULL, version = 2)[1:6]
rawToChar(serialize(1L, NULL, ascii = TRUE, version = 2)[1:4])
identical(unserialize(serialize(x, NULL, version = 2)), x)
identical(unserialize(serialize(x, NULL, xdr = FALSE)), x)
# A file connection opened for binary reading sees a gzip file's raw bytes,
# while one opened for text decompresses it.
f <- tempfile(fileext = ".gz")
con <- gzfile(f, "w"); writeLines(c("one", "two"), con); close(con)
readBin(f, "raw", 2)
con <- file(f, "rb"); readBin(con, "raw", 2); close(con)
readLines(f)
con <- file(f); summary(con)$class; close(con)
# rep keeps an S4 object's class.
setClass("Tagged", contains = "numeric")
t <- new("Tagged", c(1, 2))
r <- rep(t, 2)
class(r)
isVirtualClass("Tagged")
r@.Data
