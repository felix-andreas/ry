# type.convert() and scan() of numbers test whether a field is blank character by character, so
# a field that starts with an invalid UTF-8 sequence is an error naming its bytes, while one that
# starts with a valid character is text. GNU R's message also marks the byte where its count of
# remaining bytes runs out.
f <- tempfile()
attempt <- function(expr) print(tryCatch(expr, error = conditionMessage))
writeBin(as.raw(c(0x31, 0x2c, 0xc3, 0x0a, 0xe9, 0x74, 0xe9, 0x2c, 0x32, 0x0a)), f)
attempt(scan(f, what = complex(), sep = ",", quiet = TRUE))
attempt(scan(f, what = double(), sep = ",", quiet = TRUE))
attempt(scan(f, what = "", sep = ",", quiet = TRUE))
attempt(read.csv(f, header = FALSE))
writeBin(as.raw(c(0x61, 0x2c, 0x62, 0x0a, 0xe9, 0x74, 0xe9, 0x2c, 0x30, 0x2e, 0x33, 0x0a)), f)
attempt(read.csv(f))
attempt(read.csv(f, stringsAsFactors = TRUE))
for (s in c("\xe9t\xe9,0.333", "\xe9,0.333", "\xe9abcdef", "\xe9\xe9abcdef", "a\xe9b\xe9cdefgh",
            "\xc3", "\xc3\xa9\xff", " \xe9xyzwvu", "\xf0\x9f\x98abcdef", "\xe9\x80abcdef"))
  attempt(type.convert(c(s, "1"), as.is = TRUE))
attempt(type.convert(c("NA", "\xe9"), as.is = TRUE, na.strings = "\xe9"))
unlink(f)

# Factor codes follow the sorted levels, and white space of any width is blank.
str(type.convert(c("b", "a", NA, "b", "ä"), as.is = FALSE))
type.convert(c("  ", "  "), as.is = TRUE)
type.convert(paste0("1", strrep(" ", 5000)), as.is = TRUE)
