# deparse() measures a line in bytes when it decides where to break it, so elements and names
# with non-ASCII characters break lines sooner than their character count suggests.
print(deparse(rep_len(c("b c", "é"), 40)))
print(deparse(rep_len(c("日本", "a"), 30)))
dput(list(x = rep_len(c("日本", "a"), 30)))
print(deparse(rep_len("éééééééééé", 8)))
print(deparse(rep_len(c("é", "ab"), 40), width.cutoff = 20L))
x <- 1:12
names(x) <- rep_len(c("ü", "日"), 12)
print(deparse(x))
print(deparse(quote(f("日本日本日本", "日本日本日本", "日本日本日本", "日本"))))
print(deparse(function(x) "éééééééééééééééééééééééééé"))
