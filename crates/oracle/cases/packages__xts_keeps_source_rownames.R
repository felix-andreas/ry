# oracle: requires xts
# Converting a matrix with date row names to xts leaves the matrix's own row names alone, so a
# second conversion of the same matrix still finds them.
suppressMessages(library(xts))
m <- matrix(1:4, 4, 1, dimnames = list(c("2017-01-01", "2017-01-02", "2017-01-03", "2017-01-04"), "x"))
x <- as.xts(m)
rownames(m)
print(x)
print(first(m, "2 days"))
print(periodicity(m))
invisible()
