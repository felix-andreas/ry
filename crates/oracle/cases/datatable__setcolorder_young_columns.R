# oracle: requires data.table
# setcolorder permutes a table's columns with memcpy through DATAPTR_RO. The columns `:=` just
# appended are young, and the permutation moves them from the end of the column list to its
# start, across the 128-element cards the collector remembers young children by, so the minor
# collection after it must still find every column.
library(data.table)
n <- 200L
ids <- sprintf("id%03d", seq_len(n))
present <- ids[-c(2, 50)]
dt <- as.data.table(c(
  list(Date = 1:10),
  setNames(lapply(present, function(i) as.numeric(0:9)), present)
))
invisible(gc())
missing <- setdiff(ids, names(dt))
dt[, (missing) := 0]
setcolorder(dt, c("Date", sort(ids)))
invisible(gc(full = FALSE))
for (k in 1:5) junk <- lapply(1:2000, function(i) character(100))
cat(ncol(dt[, -1L]), "\n")
cat(identical(names(dt), c("Date", sort(ids))), "\n")
cat(sum(vapply(dt, sum, numeric(1))), "\n")
