# A text connection reads a string's bytes, an invalid one included, and refuses a string
# declared as bytes unless `encoding = "bytes"` asks for the bytes themselves.
x <- "\xc3"
r <- readLines(textConnection(x)); print(charToRaw(r)); print(Encoding(r))
y <- "\xc3"; Encoding(y) <- "bytes"
print(tryCatch(textConnection(y), error = function(e) conditionMessage(e)))
tc <- textConnection(y, encoding = "bytes"); print(charToRaw(readLines(tc))); close(tc)
l <- "\xe4"; Encoding(l) <- "latin1"
tc <- textConnection(l, encoding = "bytes"); print(charToRaw(readLines(tc))); close(tc)
tc <- textConnection(l); print(charToRaw(readLines(tc))); close(tc)
tc <- textConnection(c("a", NA)); print(readLines(tc)); close(tc)

# An output text connection marks each non-ASCII line UTF-8.
tc <- textConnection("o", "w", local = TRUE); writeLines(c("a", "ä", "\xc3"), tc); close(tc)
print(Encoding(o)); print(charToRaw(o[3]))
print(Encoding(capture.output(cat("a\nä\n"))))

# make.names: an undecodable first character shows the text, a later one counts the name.
for (name in list("a\xc3", "\xc3a", c("ok", "ab\xc3cd"), ".\xc3", "\xe4\xf6\xfcabcdefghijklmnop", "1\xc3", "", ".", "ä\xc3"))
  print(tryCatch(make.names(name), error = function(e) conditionMessage(e)))

# A latin1 string translates as Windows-1252; a byte that code page leaves undefined shows as <xx>.
z <- rawToChar(as.raw(c(0x41, 0x80:0x9f, 0xa0, 0xff))); Encoding(z) <- "latin1"
u <- enc2utf8(z); print(utf8ToInt(u)); print(Encoding(u)); print(nchar(z)); print(z)
print(z == u); print(paste0(z, "!")); print(nchar(z, "bytes"))
print(utf8ToInt(iconv(z, "latin1", "UTF-8"))[2:6])
q <- "\x93quoted\x94 \x80 5"; Encoding(q) <- "latin1"; print(q); cat(q, "\n"); print(toupper(q)); print(nchar(q))
w <- "\x81b"; Encoding(w) <- "latin1"; print(w); print(nchar(w)); print(utf8ToInt(enc2utf8(w))); print(w == "\u0081b")

# A gzip file names Unix in its header.
f <- tempfile(); saveRDS(1:10, f); print(readBin(f, "raw", 10))
con <- gzfile(f, "w"); writeLines("a", con); close(con); print(readBin(f, "raw", 10))
unlink(f)
