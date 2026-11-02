# Long integer sequences are compact, so building one near the integer limit costs no memory.
x <- seq_len(2^31 - 1)
y <- 1:(2^31 - 1)
print(c(length(x), length(y), length((2^31 - 1):1)))
print(seq_along(seq_len(2e6))[c(1, 2e6)])
w <- 1:2e6
w[3] <- 0L
print(c(sum(w[1:5]), length(w), w[2e6]))
print(sum(as.numeric(3e6:1)))
