# oracle: requires data.table
# Native code stores strings into a character column in place, one cell at a time
# (`SET_STRING_ELT`). The column may have survived collections while the strings are fresh, so
# every collection after a store must still find them, and a replaced string that nothing else
# holds goes away without harm.
suppressMessages(library(data.table))

# A column that survived collections takes fresh strings.
frame <- data.table(id = 1:2000, label = rep("a", 2000))
invisible(gc())
invisible(gc())
rows <- seq(1L, 2000L, by = 7L)
for (round in 1:3) {
    set(frame, i = rows, j = "label", value = paste0("k", round, "_", rows))
    invisible(gc())
    print(frame$label[c(1, 2, 8, 1996)])
}
print(length(unique(frame$label)))
frame[id %% 3L == 0L, label := paste0("m", id)]
invisible(gc())
print(head(frame$label, 10))
print(tail(frame$label, 4))

# A large column, whose stores the collector tracks by region.
large <- data.table(label = rep("z", 200000L))
invisible(gc())
invisible(gc())
set(large, i = c(1L, 100000L, 200000L), j = "label",
    value = c(paste0("first", 1), paste0("middle", 2), paste0("last", 3)))
invisible(gc())
print(large$label[c(1L, 2L, 100000L, 200000L)])
print(sum(nchar(large$label)))
for (round in 1:3) {
    set(large, i = 150000L + round, j = "label", value = paste0("round", round))
    invisible(gc())
}
print(large$label[150000L + 0:4])

# Every string of a column is replaced, and the old ones were held by the column alone.
column <- data.table(label = paste0("unique", 1:50))
set(column, j = "label", value = rep(NA_character_, 50L))
invisible(gc())
print(sum(is.na(column$label)))
set(column, i = 1:50, j = "label", value = paste0("again", 50:1))
invisible(gc())
print(column$label[c(1, 25, 50)])

# The same string stored into every cell, and a cell stored twice.
same <- data.table(label = character(100))
set(same, j = "label", value = "one")
set(same, i = 5L, j = "label", value = "two")
set(same, i = 5L, j = "label", value = "three")
print(table(same$label))

# One string recycled over several cells, and a copy that keeps the old strings.
before <- copy(frame)
set(frame, i = 1:6, j = "label", value = "x")
print(frame$label[1:8])
print(before$label[1:8])
