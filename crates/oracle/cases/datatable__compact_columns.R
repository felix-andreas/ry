# oracle: requires data.table
# data.table's C code writes into vectors it duplicated; with compact sequences as columns, the
# original sequences must keep their values.
library(data.table)
x <- 1:3
frev(x)
x
setfrev(x)
x
X <- list(a = 4:1, b = c(0.1, 0.2, 0.3, 0.4))
A <- setkey(as.data.table(X), a)
X$a
B <- setDT(X, key = "a")
all.equal(A, B)
B
dt <- data.table(i = 1:100, v = as.numeric(100:1))
setorder(dt, v)
head(dt, 3)
identical(1:100, seq_len(100))
