# Queries outside ry's engine still run on data.table itself, with its own errors and warnings.
library(data.table)
dt <- data.table(g = c("a", "b", "a"), x = 1:3, y = c(0.5, 1.5, 2.5))
setkey(dt, g)
dt["a"]
key(dt[x > 1L])
dt[, .(first = y[1L]), by = g]
dt[, .(m = median(y)), by = g]
dt[, sum(y)]
tryCatch(dt[, nope], error = function(e) conditionMessage(e))
withCallingHandlers(dt[, .(s = sum(x)), by = g], warning = function(w) cat("warning:", conditionMessage(w), "\n"))
big <- data.table(g = c(1L, 1L), v = c(.Machine$integer.max, 1L))
big[, .(s = sum(v)), by = g]
dt[, w := cumsum(x)]
dt
invisible(NULL)
