# Growing a vector by assignment puts names first, then the other attributes in order.
x <- list(a = 1)
attr(x, "where") <- list(a = 2)
x[["b"]] <- 3
print(names(attributes(x)))
z <- c(a = 1)
attr(z, "w") <- 1
z[3] <- 2
print(attributes(z))

# Infinite subscripts select NA and cannot be mixed with negative ones.
y <- 1:4
print(y[Inf])
print(y[-Inf])
print(y[c(Inf, 2)])
print(tryCatch(y[c(-Inf, -1)], error = conditionMessage))
