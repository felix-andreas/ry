# col2rgb() reads anything but numbers and strings as strings: a list's elements, a factor's
# codes, a logical's "TRUE".
x <- lapply(list("#1f77b4", "#ff7f0e"), function(x) structure(x, class = "colorway"))
print(col2rgb(x, alpha = TRUE))
print(col2rgb(list(a = "red", b = 3L)))
print(col2rgb(list(1, "blue")))
print(col2rgb(NA))
print(col2rgb(factor("red")))
print(tryCatch(col2rgb(TRUE), error = conditionMessage))
