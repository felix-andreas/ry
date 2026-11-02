# Display widths of control characters, and formatC with a negative width, which left-justifies.
formatC(3.5, width=-6); formatC(c("a","bb"), width=-4)
nchar("a\tb", type="width")
nchar(c("a\tb", "a\177b", "a\nb"), type = "width")
formatC(c(-1.25, 3), width = -7, format = "f", digits = 1)
