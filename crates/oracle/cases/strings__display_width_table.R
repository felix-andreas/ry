# The columns a character takes come from GNU R's own tables, which follow no Unicode release
# exactly: a character is wide, takes no column, or takes one.
codes <- c(1:0xD7FF, 0xE000:0xFFFD, 0x10000:0x3FFFD, 0xE0000:0xE0FFF)
chars <- intToUtf8(codes, multiple = TRUE)
widths <- nchar(chars, "width")
runs <- function(hit) {
    v <- codes[hit]
    breaks <- c(TRUE, diff(v) != 1)
    writeLines(strwrap(paste(sprintf("%X-%X", v[breaks], v[c(breaks[-1], TRUE)]), collapse = " "), 100))
}
table(widths)
cat("no column:\n"); runs(widths == 0)
cat("two columns:\n"); runs(widths == 2)

# Everything that pads or aligns text counts the same columns.
x <- c("plain", "café", "é", "日本語", "\U0001F600!", "a­b", "؀x", "tab\there", "☰", "")
nchar(x, "width")
format(x)
formatC(x, width = 8)
formatC(x, width = -8, flag = "-")
formatC(x, width = 8, flag = "0")
sprintf("%6s|%-6s|", x, x)
strtrim(x, 3)
print(x, quote = FALSE)
data.frame(text = x, n = seq_along(x))
m <- matrix(x[1:6], 2, dimnames = list(c("日", "r2"), c("a", "éé", "\U0001F600")))
m
noquote(m)
names(x) <- x
x[1:5]
encodeString(x, width = 8, justify = "right")
encodeString(x, width = NA, quote = '"', justify = "centre")
