# `cut` and `.bincode` on integer input, which a bug once read as raw doubles.
print(cut(1:5, c(0.996, 3, 5.004)))
print(.bincode(1:5, c(0, 3, 6), TRUE, FALSE))
print(.bincode(c(0, 1, 3, 6, NA), c(1, 3, 6), FALSE, TRUE))
print(.bincode(c(1, 3), c(1, 3), TRUE, FALSE))
print(tryCatch(.bincode(1, c(3, 1), TRUE, TRUE), error = conditionMessage))
str(parallel::splitIndices(5L, 2))
print(table(cut(c(1L, 5L, 9L, 10L), breaks = 3)))
NULL
