# `attributes<-` checks the whole value before it clears anything.
x <- structure(1:3, class = "keep")
print(tryCatch(attributes(x) <- list(1, b = 2), error = conditionMessage))
print(tryCatch(attributes(x) <- list(a = 1, 2), error = conditionMessage))
print(tryCatch(attributes(x) <- list(1), error = conditionMessage))
print(tryCatch(attributes(x) <- 1, error = conditionMessage))
print(attributes(x))

# Empty dimnames components are stored as NULL.
r <- integer(0)
dim(r) <- 0L
dimnames(r) <- list(character())
str(dimnames(r))
