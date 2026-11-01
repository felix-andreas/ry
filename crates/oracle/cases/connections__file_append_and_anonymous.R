# file.append() recycles its arguments and reports a missing source as FALSE.
a <- tempfile(); b <- tempfile(); c <- tempfile()
writeLines("one", a); writeLines("two", b); writeLines("three", c)
file.append(a, c(b, c))
readLines(a)
file.append(a, tempfile())
tryCatch(file.append(character(), b), error = conditionMessage)
file.append(a, character())

# An anonymous file() is a scratch buffer: what is written can be read back.
con <- file(open = "w+b")
cat("hello", file = con)
readChar(con, 5, useBytes = TRUE)
close(con)
con <- file()
writeLines(c("x", "y"), con)
readLines(con)
invisible(close(con))
TRUE
