# `:=` on data.tables under ry's runtime: in place, by reference, and only as data.table would.
library(data.table)
dt <- data.table(g = c("a", "b", "a", "c", "b"), x = 1:5, y = c(1.5, 2.5, NA, 4.5, 5.5))
alias <- dt
dt[, z := y * 2]
dt[x > 2L, y := 0]
dt[, x := x + 10L]
dt[, s := sum(x), by = g]
identical(alias, dt)
dt
truelength(dt) >= length(dt)

# A query result is a new table: changing it leaves the source alone.
r <- dt[x > 12L]
r[, x := 0L]
dt$x

# copy() keeps changes apart.
d2 <- copy(dt)
d2[, y := -1]
dt$y

# Filters on `==` and `%in%` build data.table's auto index on the source.
dt[g == "a"]
indices(dt)
dt[g %in% c("a", "c")]

# Type mismatches warn exactly as data.table does.
dt[1L, x := 2.5]
dt$x
