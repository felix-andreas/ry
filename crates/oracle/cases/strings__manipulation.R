s <- "The quick brown fox"
toupper(s); nchar(s); substr(s, 5, 9); strsplit(s, " ")[[1]]
paste("a", "b", sep = "-"); paste0("x", 1:3); paste(c("a", "b"), collapse = "+")
sprintf("%5.2f|%-6s|%03d|%e|%x", pi, "ab", 7L, 12345.678, 255L)
sprintf("%s has %d chars", "word", nchar("word"))
gsub("o", "0", s); sub("(\\w+) (\\w+)", "\\2 \\1", s); regmatches(s, regexpr("[a-z]+", s))
grepl("^The", c(s, "the end")); grep("o", c("one", "two", "six"), value = TRUE)
trimws("  padded  "); formatC(3.14159, digits = 3, format = "f"); formatC(42, width = 8, flag = "0")
format(1234567.891, big.mark = ","); format(TRUE); format(c(1, 10, 100))
sprintf("%08.3f", -3.5); toupper(letters[1:5]); casefold("ABC", upper = FALSE)
chartr("abc", "xyz", "aabbcc"); rev(strsplit("hello", "")[[1]]); startsWith(s, "The")
sprintf("%%"); nchar(c("a", NA, "abc")); substring("abcdef", 1:3, 3:5)
