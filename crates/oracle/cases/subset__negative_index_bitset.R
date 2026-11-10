# Negative subscripts drop elements: a mask over the vector's elements, inline for short vectors
# and on the heap past 256.
x <- c(a = 1, b = 2, c = 3, d = 4)
print(x[-2])
print(x[c(-1, -1, -4)])
print(x[c(0, -3)])
print(x[-10])
print(x[-2.9])
print(x[c(-0.5, -2)])
print(x[c(0, 0)])
long <- seq_len(300)
print(sum(long[-c(1, 64, 65, 128, 256, 257, 300)]))
print(length(long[-(1:299)]))
l <- list(1, "a", TRUE)
str(l[-1])
print(letters[-(1:24)])
print(tryCatch(x[c(-1, 2)], error = conditionMessage))
print(tryCatch(x[c(-1, NA)], error = conditionMessage))
y <- 1:5
y[-1] <- 0L
print(y)
# Named arguments to builtins match exactly, then partially, then by position.
print(round(digits = 1, x = 2.345))
print(rep_len(length.out = 3, x = 1:2))
print(tryCatch(round(2.5, foo = 1), error = conditionMessage))
print(seq_len(length.out = 3))
print(tryCatch(seq_len(len = 3), error = conditionMessage))
print(tryCatch(nchar(type = "chars", "abc", typ = "bytes"), error = conditionMessage))
