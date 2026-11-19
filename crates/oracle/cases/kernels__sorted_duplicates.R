# duplicated(), unique() and anyDuplicated() find the duplicates of a double vector that never
# decreases by comparing neighbours, and hash any other vector, so both paths must agree on
# signed zeros, infinities, NA against NaN, fromLast, and a vector that descends only at its end.
x <- c(-Inf, -1, -0, 0, 0, 1, 1, 1, 2.5, Inf, Inf)
for (from_last in c(FALSE, TRUE)) {
  print(duplicated(x, fromLast = from_last))
  print(unique(x, fromLast = from_last))
  print(anyDuplicated(x, fromLast = from_last))
}
print(1 / unique(c(-0, 0)))
print(1 / unique(c(-0, 0), fromLast = TRUE))
print(unique(c(1, 2, NA, NA)))
print(unique(c(NaN, NA, NA, NaN)))
print(duplicated(c(1, 1, NaN, NaN)))
print(unique(c(1, 2, 3, 3, 2)))
print(anyDuplicated(c(1, 2, 3)))
print(unique(numeric(0)))
set.seed(1)
s <- sort(round(runif(1e5, 0, 1000), 1))
cat(sum(duplicated(s)), sum(which(duplicated(s, fromLast = TRUE))), anyDuplicated(s),
    anyDuplicated(s, fromLast = TRUE), length(unique(s)), "\n")
u <- runif(2e5)
cat(anyDuplicated(u), length(unique(u)), anyDuplicated(c(u, u[7])), "\n")
i <- sample.int(1e9, 1e5, replace = TRUE)
cat(sum(duplicated(i)), anyDuplicated(c(i, i[3])), "\n")
