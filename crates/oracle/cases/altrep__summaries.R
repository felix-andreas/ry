# sum, min and max ask a compact sequence's class first, as GNU R does.
x <- 1:100000
print(typeof(sum(x)))
print(sum(x))
print(sum(x, 1L))
print(typeof(sum(1:10)))
print(sum(1:10, na.rm = TRUE))
print(c(min(x), max(x), min(100:1), max(-5:5)))
print(sum(integer(0)))
print(sum(seq_len(46341)))
print(sum(-100000:-1))
y <- seq(1, 1e6, by = 1)
print(sum(y))
print(c(min(y), max(y)))
print(sum(1e10:(1e10 + 5)))
z <- 1:5
z[2] <- 10L
print(sum(z))
print(sum(x, 2.5, NA, na.rm = TRUE))
print(sum(c(1L, NA), 1:3))
