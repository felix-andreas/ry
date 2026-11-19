# A made name is native text. GNU R's answer for the same text in two encodings depends on
# CHARSXP addresses (strings.rs, `make_unique_interned`), so no line mixes them.
u <- "é"
l <- iconv(u, "UTF-8", "latin1")
nat <- rawToChar(charToRaw(u))
show <- function(x) {
  print(x)
  print(Encoding(x))
}
show(make.unique(c(NA, NA, "NA", "NA.1", NA)))
show(make.unique(c(u, u, u), sep = "_"))
show(make.unique(c(u, u, "a", "a", u)))
show(make.unique(c(l, l, l)))
show(make.unique(c("a", "a", "a.1", "a")))
show(make.unique(c(nat, nat, rawToChar(charToRaw(paste0(u, ".1"))))))
show(make.unique(c(u, u, rawToChar(charToRaw(paste0(u, ".1"))))))
show(make.unique(c(u, u), sep = u))
print(charToRaw(make.unique(c(l, l))[2]))
ascii_bytes <- "x"
Encoding(ascii_bytes) <- "bytes"
show(make.unique(c(ascii_bytes, ascii_bytes)))
bytes <- u
Encoding(bytes) <- "bytes"
print(tryCatch(make.unique(c(bytes, "x")), error = conditionMessage))
print(tryCatch(make.unique(c(bytes, bytes)), error = conditionMessage))
print(tryCatch(make.unique(c("a", "a"), sep = c("x", "y")), error = conditionMessage))
