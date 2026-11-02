# Query shapes from data.table's own test suite: each answer must match data.table's, whether the
# engine answers or declines.
library(data.table)
dt <- data.table(a = c(1L, 1L, 2L, 3L), b = c(4, 5, 6, 7), c = c("x", "y", "x", "z"))
# An untagged third argument is `by`.
dt[, sum(b), a]
# `c:a` selects columns backwards.
names(dt[, c:a])
# A grouped minimum with no values left warns and returns Inf.
withCallingHandlers(
  data.table(g = c(1L, 1L, 2L), y = c(3L, NA, NA))[, min(y, na.rm = TRUE), by = g],
  warning = function(w) cat("warning:", conditionMessage(w), "\n")
)
# The minimum of an ordered factor by group.
f <- data.table(g = c(1L, 1L, 2L), o = factor(c("lo", "hi", "hi"), levels = c("lo", "hi"), ordered = TRUE))
f[, .(m = min(o)), by = g]
# A rolling join keeps every exact match and rolls only the rest.
x <- data.table(k = c(1, 1, 3, 3), v = c(1, 2, 3, 4))
i <- data.table(k = c(1, 2, 4))
x[i, on = "k", roll = TRUE]
x[i, on = "k", roll = -Inf]
x[i, on = "k", roll = "nearest"]
x[i, on = "k", roll = TRUE, mult = "first"]
# `.EACHI` with a join column in `j` reads `i`'s values.
x[i, .(k, n = .N), on = "k", by = .EACHI]
# `:=` with `nomatch`.
y <- copy(x)
y[data.table(k = 3), w := v * 2, on = "k", nomatch = NULL]
y
# Row names in the compact form are identical to `1:n`.
d <- data.frame(p = 1:3)
e <- d
attr(e, "row.names") <- 1:3
identical(d, e)
# A 1-D table subset to one element is a named vector.
t1 <- table(c("q", "q", "r"))
str(t1["q"])
str(t1[c("q", "r")])
# `rep` refuses a function, and `nzchar` of the empty symbol is FALSE.
tryCatch(rep(sum, 2), error = function(e) conditionMessage(e))
sapply(quote(list(a, )), function(x) is.symbol(x) && !nzchar(x))
# `read.table(fill = TRUE)` pads a short row's character fields with "".
text <- "a,b,c\n1,2,qq\n4,5\n\n7,8,rr\n"
read.table(text = text, sep = ",", header = TRUE, fill = TRUE, blank.lines.skip = FALSE, stringsAsFactors = FALSE)
