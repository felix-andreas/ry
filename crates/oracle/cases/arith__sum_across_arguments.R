# Each argument of sum() is summed in extended precision, but the running total across
# arguments is a double, so the order of arguments shows through.
print(sum(1e308, 1e308, -1e308))
print(sum(c(1e308, 1e308, -1e308)))
print(sum(2^53, 1, 1) - 2^53)
print(sum(c(2^53, 1, 1)) - 2^53)
x <- c(1e16, 1, -1e16)
print(sum(x[1], x[2], x[3]))
print(sum(x))
print(sum(0.1, 0.2, 0.3) == 0.6)
print(sum(1:3, c(0.5, NA), na.rm = TRUE))
print(sum(1L, 2.5, TRUE))
