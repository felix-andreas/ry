# Queries whose `j` arrives through `eval()`, or with a primitive spliced in as the function, as
# data.table's own `dcast` builds them, run on ry's engine with data.table's results.
library(data.table)
dt <- data.table(
  g = c("b", "a", "b", "c", "a", "b"),
  k = c(2L, 1L, 2L, 1L, 2L, 1L),
  x = c(4L, 1L, 3L, NA, 5L, 2L),
  y = c(0.1, 0.2, 0.3, 0.4, 0.5, 0.6)
)
j <- quote(.(s = sum(x), m = max(y)))
dt[, eval(j), by = g]
dt[, eval(j), keyby = .(g, k)]
total <- quote(sum(x, na.rm = TRUE))
dt[, eval(total)]
dt[, eval(expression(.N)), by = k]
spliced <- quote(list(x = sum(x)))
spliced[[2L]][[1L]] <- sum
dt[, eval(spliced), by = g]
spliced[[2L]][[2L]] <- quote(y)
dt[, eval(spliced), by = g]
dcast(dt, g ~ k, value.var = "x", fun.aggregate = sum)
dcast(dt, g ~ k, value.var = "y", fun.aggregate = sum)
dcast(dt, g ~ k, value.var = c("x", "y"), fun.aggregate = length)
names(attributes(dcast(dt, g ~ k, value.var = "x", fun.aggregate = sum)))
dt[, c("g", "y"), with = FALSE]
dt[, .(g, x)]
update <- quote(z := x * 2L)
dt[, eval(update)]
dt
# GForce widens an integer sum once its running total leaves the integer range, though the final
# total fits.
m <- .Machine$integer.max
running <- data.table(g = c(1L, 1L, 1L, 2L), v = c(m, 1L, -5L, 3L))
withCallingHandlers(
  sapply(running[, .(s = sum(v)), by = g], class),
  warning = function(w) cat("warning:", conditionMessage(w), "\n")
)
running$v <- c(m, -5L, 1L, 3L)
sapply(running[, .(s = sum(v)), by = g], class)
running$v <- c(-m, -1L, 5L, 3L)
withCallingHandlers(
  sapply(running[, lapply(.SD, sum), by = g], class),
  warning = function(w) cat("warning:", conditionMessage(w), "\n")
)
# Outside GForce, base `sum` widens one group's total and data.table rejects the mixed types.
wide <- data.table(g = c("a", "a", "b"), x = c(m, 1L, 5L))
tryCatch(wide[, .(s = sum(x) + 0L), by = g], error = function(e) conditionMessage(e))
# `eval(e)` whose variables name a column means that column.
named <- data.table(e = 1:3, g = c(1L, 1L, 2L), b = 7:9)
e <- quote(sum(b))
named[, eval(e), by = g]
# data.table reads `with = FALSE` from `j` before it expands `eval()`.
positions <- quote(1:2)
dt[2:3, eval(positions)]
# An empty evaluated `by` is one group.
none <- character()
dt[, max(y), by = none]
rollup(dt, j = sum(x), by = c("g", "k"))
# The same text in different encodings is one group.
utf8 <- "\u00e9"
native <- utf8
Encoding(native) <- "unknown"
latin1 <- iconv(utf8, "UTF-8", "latin1")
mixed <- data.table(s = c(utf8, native, latin1, "b"), v = 1:4)
mixed[, .(n = sum(v)), by = s]$n
# An unnamed key expression is named after its first variable.
dt[, .(n = .N), keyby = .(g, x > 2L)]
invisible(NULL)
