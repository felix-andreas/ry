# Doubles as text, through the shortest-digit shortcut and its fallbacks: subnormals, values
# whose exact digits pass 15, integers past 1e15, and options(scipen) both ways.
values <- c(0, -0, 1, -1, 0.1, 1e-5, 1e-4, 123456789012345, 1234567890123456, 1e15, 1e16,
  93290982884354896, 1e-15, 1e100, 1e-100, 1.5e-300, 1/3, 2/3, pi, -pi * 1e10, 100, 1e5,
  123456.7, 0.0001234, .Machine$double.xmax, .Machine$double.eps, 5e-324, 2.5e-310,
  NA, NaN, Inf, -Inf)
writeLines(as.character(values))
writeLines(paste(values))
cat(values, "\n")
options(scipen = 5)
writeLines(as.character(values))
cat(values, "\n")
options(scipen = -3)
writeLines(as.character(values))
options(scipen = 0, digits = 3)
cat(values, "\n")
print(round(c(1.23456, 1.23456, 1.23456), c(1, 2, 3)))
print(signif(c(a = 1.23456, b = 1.23456), 1:2))
print(round(2.5678, c(0, 1, 2)))
