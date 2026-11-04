# fft and mvfft run Singleton's mixed-radix algorithm as GNU R does, so every length gives GNU R's
# bits, not just the same values up to rounding.
bits <- function(x) {
  r <- as.numeric(writeBin(as.double(c(Re(x), Im(x))), raw()))
  sprintf("%.0f", sum(r * ((seq_along(r) %% 251) + 1)))
}
print(sprintf("%a", Re(fft(1:8 + 0.5))))
print(sprintf("%a", Im(fft(1:8 + 0.5))))
print(fft(1:8 + 0.5)[3]); print(Re(fft(1:8 + 0.5)[3]) == -4)
set.seed(7)
for (n in c(2:40, 48, 49, 50, 60, 64, 72, 77, 81, 97, 100, 101, 120, 121, 125, 128, 143, 169, 210, 243, 256, 360, 500, 512, 625, 1000, 1009, 1024, 1260, 2310, 4096, 5000, 7919, 10000)) {
  z <- complex(real = rnorm(n), imaginary = rnorm(n))
  f <- fft(z)
  cat(sprintf("n=%-6d fft %s inverse %s roundtrip %s\n", n, bits(f), bits(fft(z, inverse = TRUE)), bits(fft(f, inverse = TRUE) / n)))
}
x <- c(3, 1, 4, 1, 5, 9, 2, 6)
print(sprintf("%a", Mod(fft(x)))); print(fft(c(1, NA, 3)))
print(fft(5)); print(fft(numeric())); print(fft(1:2))
a <- array(rnorm(4 * 6 * 5), c(4, 6, 5))
cat("array", bits(fft(a)), bits(fft(a, inverse = TRUE)), "\n")
a <- array(rnorm(12 * 1 * 7), c(12, 1, 7))
cat("array with a unit dimension", bits(fft(a)), "\n")
m <- matrix(rnorm(30 * 6), 30, 6)
cat("matrix", bits(fft(m)), "\n")
cat("mvfft", bits(mvfft(m)), bits(mvfft(m, inverse = TRUE)), "\n")
cat("mvfft one row", bits(mvfft(matrix(rnorm(3), 1, 3))), "\n")
r <- try(mvfft(1:4), silent = TRUE); cat(conditionMessage(attr(r, "condition")), "\n")
print(nextn(1001)); print(convolve(1:5, c(1, 2, 3), type = "o"))
print(sprintf("%a", spec.pgram(ldeaths, plot = FALSE)$spec[1:5]))
