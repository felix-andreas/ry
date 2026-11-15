show <- function(x) {
  print(x)
  print(Encoding(x))
}
# Scalars, the shape most calls have.
show(paste("a", "b", "c"))
show(paste0("ACC", "000001"))
show(paste("x", 1L, 2.5, TRUE, sep = ";"))
show(paste("a", "", "c", sep = ""))
show(paste0("", ""))
show(paste("a", NA, "c"))
show(paste(NA_character_, NA))
show(paste0())
show(paste(sep = "-"))
show(paste("solo"))
# Recycling, empty columns and recycle0.
show(paste(c("a", "b", "c"), 1:2))
show(paste(character(0), "x"))
show(paste("x", character(0), "y", sep = "+"))
show(paste(character(0), "x", recycle0 = TRUE))
show(paste(c("a", "b"), NULL, c("y", "z"), sep = "_"))
show(paste(list(1, "b", NULL, c(2, 3))))
show(paste(factor(c("lo", "hi")), 1:2))
# Collapse.
show(paste(c("a", "b", "c"), collapse = ""))
show(paste("a", "b", collapse = "|"))
show(paste(c("a", NA), 1:2, sep = ".", collapse = ", "))
show(paste(character(0), collapse = "+"))
show(paste0(c("x", "y"), collapse = NULL))
# Encodings: UTF-8 and latin1 pieces make the result UTF-8, bytes wins over both.
utf8 <- "café"
latin1 <- iconv(utf8, "UTF-8", "latin1")
bytes <- "a\xffb"
Encoding(bytes) <- "bytes"
show(paste(utf8, "x"))
show(paste("x", latin1))
show(paste(latin1, latin1, sep = ""))
print(charToRaw(paste(latin1, "x")))
show(paste(bytes, "x"))
print(charToRaw(paste(bytes, latin1)))
show(paste(c("plain", utf8), collapse = "-"))
show(paste(c("plain", "ascii"), collapse = latin1))
print(charToRaw(paste(c("x", bytes), collapse = latin1)))
show(paste(utf8, NA))
# Errors.
print(tryCatch(paste("a", sep = NA_character_), error = conditionMessage))
print(tryCatch(paste("a", sep = character(0)), error = conditionMessage))
print(tryCatch(paste("a", collapse = NA_character_), error = conditionMessage))
print(tryCatch(paste("a", collapse = 1), error = conditionMessage))
