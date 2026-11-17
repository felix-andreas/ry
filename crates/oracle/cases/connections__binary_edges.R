# Edges of binary input and output: long double, the UTF-8 forms readChar() accepts, the
# "invalid multibyte string" message of writeChar(), string limits of readBin(), and a
# connection whose encoding cannot convert.
t_ <- function(expr) {
  r <- withVisible(tryCatch(withCallingHandlers(expr, warning = function(w) { cat("warning:", conditionMessage(w), "\\n"); invokeRestart("muffleWarning") }), error = function(e) { cat("error:", conditionMessage(e), "\\n"); invisible(NULL) }))
  if (r$visible) print(r$value) else cat("(invisible)\\n")
}
f <- tempfile()
x <- c(0, 1, -1, 1.5, pi, 1e300, -1e-300, 5e-324, 2.2250738585072014e-308, Inf, -Inf, NaN, NA, .Machine$double.xmax, 0.1, 123456789.123456789)
r <- writeBin(x, raw(), size = 16); print(r)
t_(identical(readBin(r, "double", length(x), size = 16), x))
t_(readBin(r, "double", length(x), size = 16))
t_(writeBin(x, raw(), size = 16, endian = "big")); t_(readBin(writeBin(x, raw(), size = 16, endian = "big"), "double", 20, size = 16, endian = "big"))
t_(readBin(as.raw(c(1:8, 0xff, 0x7f, rep(0, 6))), "double", 1, size = 16)); t_(readBin(as.raw(c(rep(0, 7), 0x80, 0xff, 0x7f, rep(0, 6))), "double", 1, size = 16))
t_(readBin(as.raw(c(rep(0xff, 8), 0xfe, 0x7f, rep(0, 6))), "double", 1, size = 16)); t_(readBin(as.raw(c(rep(0xff, 8), 0xff, 0x43, rep(0, 6))), "double", 1, size = 16))
t_(readBin(as.raw(c(1, rep(0, 6), 0x80, 0xff, 0x3f, rep(0, 6))), "double", 1, size = 16) == 1); t_(readBin(as.raw(c(0, 4, rep(0, 5), 0x80, 0xff, 0x3f, rep(0, 6))), "double", 1, size = 16) == 1); t_(readBin(as.raw(c(0, 0x0c, rep(0, 5), 0x80, 0xff, 0x3f, rep(0, 6))), "double", 1, size = 16) - 1)
t_(sprintf("%a", readBin(as.raw(c(rep(0xff, 8), 0x01, 0x3c, rep(0, 6))), "double", 1, size = 16))); t_(sprintf("%a", readBin(as.raw(c(rep(0, 7), 0x80, 0xce, 0x3b, rep(0, 6))), "double", 1, size = 16)))
for (b in list(c(0xf5, 0x80, 0x80, 0x80), c(0xf7, 0xbf, 0xbf, 0xbf), c(0xf4, 0x8f, 0xbf, 0xbf), c(0xef, 0xbf, 0xbf), c(0xe0, 0x9f, 0x80), c(0xf0, 0x8f, 0x80, 0x80), c(0xf8, 0x88, 0x80, 0x80, 0x80), c(0xc1, 0x80), c(0xed, 0x9f, 0xbf), c(0xed, 0xa0, 0x80), c(0xc3))) {
  writeBin(as.raw(c(b, 0x61)), f); x <- NULL; t_(x <- readChar(f, 2)); if (!is.null(x)) print(charToRaw(x)) }
con <- file(f, encoding = "foo"); t_(open(con)); t_(close(con)); con <- file(f, encoding = "foo"); t_(open(con, "rb")); t_(close(con))
con <- file(f, encoding = "foo"); t_(readChar(con, 1)); t_(close(con)); con <- file(f, encoding = "foo"); t_(scan(con, "", quiet = TRUE)); t_(close(con))
t_(writeChar("a\xc3", raw(), nchars = 2)); t_(writeChar("ab\xe2\x82", raw(), nchars = 3)); t_(writeChar("\xf4\x90\x80\x80x", raw(), nchars = 2)); t_(writeChar("\xffabcdäfg\xfe", raw(), nchars = 2)); t_(writeChar("\xffabc€fg\xfe", raw(), nchars = 2))
x <- "a\xe4"; Encoding(x) <- "bytes"; t_(writeChar(x, raw(), nchars = 1)); t_(writeChar(x, raw(), nchars = 2, useBytes = TRUE)); t_(writeChar("a", raw(), eos = x)); t_(writeBin(x, raw())); t_(writeChar(c(x, "a"), f, nchars = c(1L)))
t_(writeChar("abc", f, nchars = c(1L, 2L))); t_(writeChar("abc", f, eos = c("a", "b"))); t_(writeChar("abc", f, eos = NA)); t_(writeChar("abc", f, eos = NA_character_)); t_(readBin(f, "raw", 20))
t_(writeChar(c("abc", "de"), f, nchars = c(1L, NA))); t_(readBin(f, "raw", 20)); t_(writeChar(c("abc", "de"), raw(), nchars = c(1L, NA))); t_(writeChar(c("abc", "d\xffe"), f, nchars = c(2L, 3L))); t_(readBin(f, "raw", 20))
# GNU R never sets the byte after a string it breaks at 10000 bytes, so that string's length
# differs between machines (10000 or more) and it may not be valid text. The case looks at its
# first 10000 bytes only.
writeBin(as.raw(c(rep(0x61, 10005), 0, 0x62, 0)), f); con <- file(f, "rb"); x <- NULL; t_(x <- readBin(con, "character", 5)); print(nchar(x[-1])); print(all(charToRaw(x[1])[1:10000] == as.raw(0x61))); close(con)
writeBin(as.raw(c(0x61, 0, 0x62)), f); con <- file(f, "rb"); t_(readBin(con, "character", 5)); t_(readBin(con, "character", 5)); close(con)
t_(readBin(as.raw(1:8), "integer", 2, signed = FALSE)); t_(readBin(as.raw(1:8), "double", 1, signed = FALSE)); t_(readBin(as.raw(1:8), "raw", 1, signed = FALSE)); t_(readBin(as.raw(1:8), "logical", 1, size = 1, signed = FALSE)); t_(readBin(as.raw(1:16), "complex", 1, signed = FALSE)); t_(readBin(as.raw(1:8), "character", 1, signed = FALSE))
t_(readBin(as.raw(1:8), "integer", 2, size = 3)); t_(readBin(as.raw(1:8), "integer", 2, size = 0)); t_(readBin(as.raw(1:8), "integer", 2, size = -1)); t_(readBin(as.raw(1:8), "raw", 2, size = 2)); t_(readBin(as.raw(1:8), "double", 2, size = 2)); t_(readBin(as.raw(1:8), "character", 2, size = 2))
t_(writeBin(1:2, raw(), size = 3)); t_(writeBin(as.raw(1:2), raw(), size = 2)); t_(writeBin(1i, raw(), size = 8)); t_(writeBin(1.5, raw(), size = 2)); t_(writeBin("a", raw(), size = 7)); t_(writeBin(TRUE, raw(), size = 1)); t_(writeBin(NA, raw(), size = 8)); t_(writeBin(NA_integer_, raw(), size = 2))
t_(writeBin(list(1), raw())); t_(writeBin(NULL, raw())); t_(writeBin(quote(a), raw())); t_(writeBin(1L, stdout())); t_(writeBin(1L, 99L)); 
