# A long `1:n` or `seq_len(n)` must not allocate its elements until something reads them.
x <- seq_len(2^31 - 1)
length(x)
x[c(1, 2^31 - 1)]
y <- 3e6:1
c(length(y), head(y), tail(y, 2))
z <- 1:2000000
z[2] <- 10L
list(z[1:3], typeof(z), length(z))
identical(1:2e6, seq_len(2e6))
sum(seq_len(2e6))
identical(unserialize(serialize(1:2e6, NULL)), 1:2e6)
