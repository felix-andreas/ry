# Arithmetic reads a compact sequence's elements from its start and increment. These cover
# both kinds, both directions, recycling against a longer or shorter operand, integer overflow,
# and the functions that take a sequence beside a plain vector.
x <- 1:6
x * 2L
x + c(10, 20)
-x
x / 4
x %/% 4L
x %% 4L
2^x
y <- 6:1
x - y
y * 0.5
d <- as.double(3:8)
d * d
d - x
(1:4) * c(1.5, 2.5, 3.5, 4.5, 5.5, 6.5, 7.5, 8.5)
s <- seq_len(5)
sum(c(1.1, 2.2, 3.3, 4.4, 5.5) * exp(-0.05 * s))
big <- (2147483640L):(2147483647L)
suppressWarnings(big + 1L)
big - 1L
-(5:-5)
(1:3) + 1i
abs(-3:3)
sqrt(as.double(1:4))
(1:5) == c(1, 3, 3, 5, 0)
(1:5) > 2.5
m <- matrix(1:6, 2)
m * 1:2
cumsum(1:5) * 1:5
