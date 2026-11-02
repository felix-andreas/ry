# The ASCII format escapes spaces, `?`, quotes and control characters, and reading it back must
# undo every escape, including `\?`, which R's own string literals do not accept.
x <- list(xml = "<?xml version=\"1.0\"?>\n<a>\t\\b</a>", bytes = "\001\177")
text <- serialize(x, NULL, ascii = TRUE)
identical(unserialize(text), x)
f <- tempfile()
con <- file(f, "w")
serialize(x, con, ascii = TRUE)
close(con)
con <- file(f, "rb")
y <- unserialize(con)
close(con)
identical(y, x)
y$xml
