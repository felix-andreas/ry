# A long `1:n` or `seq_len(n)` must not allocate its elements until something reads them, and reading a few of them must not allocate the rest.
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
# Reading two elements of `x` above must not expand it into 8 GB, so the peak resident set
# stays far below that (Linux reports it in kB as `VmHWM`).
status <- if (file.exists("/proc/self/status")) readLines("/proc/self/status") else character()
peak <- as.numeric(sub("^VmHWM:\\s*(\\d+).*", "\\1", grep("^VmHWM:", status, value = TRUE)))
length(peak) == 0 || peak < 1e6
