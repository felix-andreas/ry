# oracle: requires data.table
# A grouped `j` that GForce does not optimize, which data.table's C code evaluates once per
# group: the call runs as compiled code from its third evaluation, so every query here has
# enough groups to reach it, and each must match the tree walker's results.
library(data.table)
n <- 120L
DT <- data.table(
  g = rep(1:24, length.out = n),
  k = rep(c("a", "b", "c"), each = 40L),
  v = ((1:n) * 37L) %% 101L / 7,
  w = ((1:n) * 13L) %% 17L
)
spread <- function(x) max(x) - min(x)
DT[, spread(v), by = g]
DT[, .(a = max(v) - min(w), b = .N * 2L), by = g]
DT[, v[1L] + w[.N], by = g]
DT[, .(first = .I[1L], group = .GRP, key = .BY$g), by = g]
DT[, .(cols = ncol(.SD), total = sum(.SD$v)), by = g]
DT[, lapply(.SD, function(x) x + 1), by = g, .SDcols = c("v", "w")][1:6]
DT[, { tmp <- v * 2; sum(tmp) }, by = g]
DT[, if (sum(w) > 30) "high" else "low", by = g]
DT[, .(s = if (.GRP %% 2L == 0L) sum(v) else -sum(w)), by = .(g, k)]
DT[, invisible(.N), by = g][1:3]
# `<<-` from `j` reaches the global environment once per group.
seen <- 0L
invisible(DT[, { seen <<- seen + 1L; .N }, by = g])
seen
# `:=` by group, whose right side data.table evaluates per group.
copied <- copy(DT)
copied[, share := v / sum(v), by = g]
round(copied$share[1:10], 6)
copied[, rank_in := frank(v, ties.method = "first"), by = k]
copied$rank_in[c(1L, 41L, 81L, 120L)]
# A `j` with a loop or `return` stays on the tree walker.
DT[, { total <- 0; for (x in v) total <- total + x; total }, by = g][1:5]
first_big <- function(x) { for (value in x) if (value > 10) return(value); NA_real_ }
DT[, first_big(v), by = g][1:5]
# The same call site in two outer calls with different data.
grouped <- function(table) table[, .(m = mean(v), n = .N), by = k]
grouped(DT)
grouped(data.table(k = rep(c("x", "y"), 10L), v = as.numeric(1:20)))
grouped(DT[w > 5])
