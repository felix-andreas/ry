# Strings declared as bytes: what reads their bytes, and the builtins that refuse to translate them. A genuine private-use character is not mistaken for a byte.
sh <- function(expr) { r <- tryCatch(expr, error = function(e) paste("E:", conditionMessage(e)), warning = function(w) paste("W:", conditionMessage(w))); print(r) }
x <- "caf\xe9"; Encoding(x) <- "bytes"
y <- "abc"; Encoding(y) <- "bytes"
print(Encoding(c(x, y)))
print(x); cat(x, "\n"); print(nchar(x, "bytes"))
sh(nchar(x)); sh(nchar(x, "chars")); sh(nchar(x, "width")); sh(nchar(y))
sh(toupper(x)); sh(tolower(y)); sh(casefold(x))
sh(substr(x, 1, 3)); sh(substring(x, 2)); sh(deparse(strsplit(x, ""))); sh(deparse(strsplit(x, "a")))
sh(paste(x, "z")); sh(paste0(x, "é")); sh(sprintf("%s!", x)); sh(format(x));
sh(enc2utf8(x)); sh(enc2native(x)); sh(iconv(x, "latin1", "UTF-8")); sh(iconv(x, "bytes", "UTF-8"))
sh(grepl("caf", x)); sh(sub("c", "C", x)); sh(gsub("\xe9", "E", x, useBytes = TRUE)); sh(regexpr("a", x))
sh(chartr("a", "A", x)); sh(trimws(x)); sh(startsWith(x, "ca")); sh(rev(utf8ToInt(x)))
sh(x == "caf\xe9"); sh(match(x, "caf\xe9")); sh(as.character(as.symbol(x)))
sh(charToRaw(x)); sh(validUTF8(x)); sh(deparse(x)); sh(sQuote(x, FALSE)); sh(encodeString(x))
sh(make.names(x)); sh(abbreviate(x, 3)); sh(strtoi(y, 16L)); sh(as.numeric(y)); sh(sort(c(x, "b"))); sh(order(c(x, "b")))
z <- ""; print(z); print(nchar(z)); print(charToRaw(z)); print(z == intToUtf8(0xf7e9))
u <- "\U10FFE9"; print(nchar(u)); print(charToRaw(u)); print(nchar(paste0(u, "x")))
v <- "a\xffb"; print(charToRaw(sub("a", "A", v, useBytes = TRUE))); print(charToRaw(paste0(v, u))); print(nchar(v, "bytes"))
w <- rawToChar(as.raw(c(0x61, 0xe9))); print(w); print(Encoding(w)); sh(nchar(w)); sh(toupper(w)); print(charToRaw(toupper("a\xe9")))
