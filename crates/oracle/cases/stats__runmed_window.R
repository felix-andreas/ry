# runmed's running medians over a sliding sorted window: ties, infinities, signed zeros, both
# end rules, the NA actions and spans from 3 to wider than the data. GNU R's na.omit result
# reads uninitialized memory, so no case pins it.
set.seed(5)
x <- round(rnorm(2000), 1)
for (k in c(3, 5, 11, 101, 1999)) cat(k, sum(runmed(x, k, algorithm = "Stuetzle") * seq_along(x)), "\n")
for (k in c(3, 11, 101)) cat(k, sum(runmed(x, k, algorithm = "Turlach") * seq_along(x)), "\n")
y <- c(3, -0, 0, Inf, -Inf, 2, 2, 2, NaN, 1, 5, -Inf, 0, 7)
print(runmed(y, 3, algorithm = "Stuetzle"))
print(runmed(y, 5, endrule = "keep", algorithm = "Stuetzle"))
print(runmed(y, 5, endrule = "constant", algorithm = "Stuetzle"))
print(tryCatch(runmed(y, 3, na.action = "fail"), error = conditionMessage))
print(runmed(c(1, 3, 2), 7))
print(runmed(sample(1:9), 9))
print(runmed(c(1, NA, NaN, 2, NA, NA, NA, 3, .Machine$double.xmax, 4), 3, algorithm = "Stuetzle"))
print(runmed(c(1, NA, NaN, 2, NA, NA, NA, 3, -.Machine$double.xmax, 4), 3, na.action = "-Big_alternate"))
print(runmed(c(5, NA, NA, NA, NA, 1, 2), 5, endrule = "keep"))
