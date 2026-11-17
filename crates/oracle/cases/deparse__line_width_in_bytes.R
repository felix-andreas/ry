# Deparse measures a line's width in bytes, so non-ASCII strings and names break lines earlier
# than their character count would.
x <- c(日本 = FALSE, b = FALSE, c = TRUE, d = FALSE, e = TRUE, f = FALSE, g = TRUE, h = FALSE, i = TRUE)
print(deparse(x))
print(deparse(list(x, 33L)))
y <- c("日本語日本語日本語", "日本語日本語日本語", "日本語日本語日本語", "日本語日本語日本語", "abc")
print(deparse(y))
dput(y)
dput(list(é = 1, ü = 2, ö = 3, aaaaaaaaaaaa = 4, bbbbbbbbbbbbbb = 5, cccccccccccccc = 6, dddd = 7))
print(deparse(quote(f("ééééééééééé", "ééééééééééé", "ééééééééééé", b))))
print(deparse(rep(c(日本 = FALSE, b = FALSE, b = FALSE), 11)))
print(deparse(y, width.cutoff = 30))
