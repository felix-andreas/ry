# A `\x` or octal escape writes a raw byte and leaves the string native; `\u` marks it UTF-8.
x <- "caf\xc3\xa9"
y <- "café"
Encoding(c(x, y, "abc"))
nchar(x, "bytes")
x == y
identical(charToRaw(x), charToRaw(y))
"\101\102" == "AB"
# encodeString pads to the widest element when width is NULL or NA.
encodeString(c("a", "bbb"), width = NULL)
encodeString(c("a", "bbb"), width = NA, justify = "right")
try(encodeString("a", width = -2))
# The radix method sorts strings in C byte order.
sort(c("b", "B", "a", "A", "_"), method = "radix")
order(c("beta", "Alpha", "alpha"), method = "radix")
# c() keeps an NA name when there is no tag, and pastes "tag.NA" when there is.
v <- c(a = 1, 2)
names(v)[2] <- NA
names(c(v))
names(c(outer = v))
# Writing a value with lines read back from a file compares equal to the literal.
f <- tempfile()
writeLines("café", f, useBytes = TRUE)
readLines(f, encoding = "unknown") == "café"
# paste joins bytes, so bytes that are not UTF-8 survive, and the result keeps the encoding.
a <- "\xfc\x8f"
charToRaw(paste0("hello", a))
Encoding(c(paste0("x", a), paste0("café", "!"), paste("a", "b")))
# Empty matrices and NA dimnames.
matrix(character(), 0, 3)
matrix(character(), 0, 0)
# strsplit() with useBytes splits UTF-8 text on the pattern's bytes and keeps the other
# characters whole.
accented <- "«Latin-1»: èé øØ é èé"
print(strsplit(accented, "é", useBytes = TRUE)[[1]])
print(strsplit(accented, "é", fixed = TRUE, useBytes = TRUE)[[1]])
print(strsplit(accented, "é", perl = TRUE, useBytes = TRUE)[[1]])

print(matrix("hello", 1, 1, dimnames = list(NA, NA)), na.print = "foo")
