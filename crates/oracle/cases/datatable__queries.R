# data.table queries that ry's runtime answers on its own engine. The output, attributes and
# keys must be exactly data.table's.
library(data.table)
set.seed(42)
n <- 40L
dt <- data.table(
  g = sample(c("a", "b", "c"), n, TRUE),
  h = sample(1:3, n, TRUE),
  x = sample(c(1:20, NA), n, TRUE),
  y = round(rnorm(n), 3),
  flag = sample(c(TRUE, FALSE, NA), n, TRUE),
  f = factor(sample(c("lo", "hi"), n, TRUE))
)
lim <- 5L

dt[x > 15]
dt[x > lim & h != 2L]
dt[order(-x, g)]
dt[!is.na(x) & y > 0][order(y)]
dt[1:3]
dt[x > 100]
dt[, .N, by = g]
dt[, .N, by = .(g, h)]
dt[, .(s = sum(x), m = mean(y)), by = g]
dt[, .(s = sum(x, na.rm = TRUE), top = max(y)), keyby = g]
key(dt[, .(m = mean(y)), keyby = .(f, g)])
dt[, .(n = .N, total = sum(y), share = mean(flag, na.rm = TRUE)), by = g][order(-total)]
dt[, x, by = g][1:5]
dt[, mean(y), by = f]
dt[, lapply(.SD, sum), by = h, .SDcols = c("x", "y")]
dt[x > 10, .(g, z = y * 2)]
dt[, .(x, y)][1:3]
dt[, c("g", "y")][1:2]
dt[, -1][1:2]
dt[, y][1:4]
dt[, .N]
sapply(dt[x > 18], class)
names(attributes(dt[x > 10]))
names(attributes(dt[, .(s = sum(y)), by = g]))
names(attributes(dt[, .(s = sum(y)), keyby = g]))
names(attributes(dt[, .(x)]))
f <- function(d, v) d[x > v, .(s = sum(y), n = .N), by = g]
f(dt, lim)
