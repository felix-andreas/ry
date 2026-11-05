# `na.print` replaces missing values in every printed form, quoted strings included.
print(matrix(c(NA, "a")), na.print = ".")
print(matrix(c(NA, "a")), na.print = ".", quote = FALSE)
print(matrix(c(NA, 1.5, 2, NA), 2), na.print = "-")
print(c(NA, "b"), na.print = "?")
print(c(NA, 3L), na.print = "")
print(tryCatch(print(1, na.print = TRUE), error = conditionMessage))
