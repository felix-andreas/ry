# oracle: requires data.table
# setorder reorders a character column in place with memcpy through DATAPTR_RO, as it does a
# double column. The strings `:=` just stored are young, and reversing the column moves them
# from its last card to its first, so the minor collection after it must still find them.
library(data.table)
dt <- data.table(k = 300:1, s = rep("old", 300L))
invisible(gc())
dt[k <= 10L, s := paste0("young", k)]
setorder(dt, k)
invisible(gc(full = FALSE))
for (k in 1:5) junk <- lapply(1:2000, function(i) character(100))
cat(dt$s[1:12], "\n")
cat(dt$k[c(1, 300)], "\n")
