# sink(type = "message") diverts messages and standard error output to a connection and flushes
# each write; cat flushes a file connection while writeLines stays buffered; enc2utf8 marks
# native non-ASCII strings; TRE minimizes lazy repetitions over all top-level alternatives when
# no submatches are asked for; `**` is `^`.
tmp <- tempfile()
con <- file(tmp, open = "w+")
sink(con, type = "message")
cat("this\n", file = stderr())
message("msg")
print(sink.number(type = "message"))
sink(NULL, type = "message")
print(sink.number(type = "message"))
print(readLines(tmp))
close(con)
con <- file(tmp, open = "w"); writeLines("w", con); print(readLines(tmp)); cat("c\n", file = con); print(readLines(tmp)); close(con)
con <- file(tmp, open = "w"); sink(con); print(1); sink(); print(readLines(tmp)); close(con)
x <- "hé"; Encoding(x) <- "unknown"; print(Encoding(enc2utf8(c("a", x, NA))))
print(Encoding(enc2utf8(iconv("hé", "UTF-8", "latin1"))))
print(regexpr("a.*?Z|a.*?Y|Q", "aXbYcXdZ"))
print(gregexpr("Q|a.*?Y|a.*?Z", "aXbYcXdZaY")[[1]])
print(sub("(a).*?Z|(a)(.*?)Y|Q", "<\\1\\2\\3>", "aXbYcXdZ"))
print(2 ** 3); print(quote(a ** b))
