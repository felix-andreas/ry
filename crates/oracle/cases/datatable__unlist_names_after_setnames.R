# oracle: requires data.table
# `unlist()` of a one-row table gives a vector with its own names: `setnames()` rewrites the
# table's names vector in place, and a vector unlisted before the rename keeps the old names.
suppressMessages(library(data.table))
rename <- function() {
    table <- data.table(a = 1, b = 2)
    before <- unlist(table)
    setnames(table, "a", "z")
    list(before = before, after = unlist(table), names = names(table))
}
for (i in 1:3) str(rename())

cat("-- a list made into a table by reference and renamed\n")
relabel <- function() {
    row <- list(a = 1L, b = 2L)
    before <- unlist(row)
    setDT(row)
    setnames(row, c("p", "q"))
    list(before, unlist(row))
}
for (i in 1:3) str(relabel())

cat("-- the unlisted vector's names change without the table's\n")
own <- function() {
    table <- data.table(a = "x", b = "y")
    flat <- unlist(table)
    names(flat)[1] <- "changed"
    list(flat, names(table))
}
for (i in 1:3) str(own())
