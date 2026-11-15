# `%%` and `%/%` on doubles, bit for bit: the sign of a zero result, whole numbers up to 2^53,
# divisors on both sides of 2^63 (where GNU R stops correcting in extended precision), tiny
# dividends whose quotient underflows, infinities, and which NaN or NA payload and sign
# propagates. The scalar and the vector kernels must agree.
bits <- function(x) vapply(x, function(v) paste(sprintf("%02x", as.integer(rev(writeBin(v, raw())))), collapse = ""), "")
payload <- readBin(as.raw(c(0x05, 0, 0, 0, 0, 0, 0xf8, 0x7f)), "double")
values <- c(0, -0, 1, -1, 3, -3, 6, -6, 7, 86400, -1.7e9, 2^31, 2^52, 2^53 - 1, -(2^53 - 1), 2^53, 2^53 + 2,
            2^62, 2^63, -2^63, 2^64, 1e17, -1e300, 0.5, -1.5, 5.5, 365.25, 1e-300, -1e-300, .Machine$double.eps,
            Inf, -Inf, NaN, -NaN, NA, -NA_real_, payload)
grid <- expand.grid(a = seq_along(values), b = seq_along(values))
a <- values[grid$a]; b <- values[grid$b]
remainder <- suppressWarnings(a %% b); quotient <- suppressWarnings(a %/% b)
scalar <- mapply(function(x, y) suppressWarnings(c(x %% y, x %/% y)), a, b)
stopifnot(identical(bits(remainder), bits(scalar[1, ])), identical(bits(quotient), bits(scalar[2, ])))
writeLines(paste(bits(a), bits(b), bits(remainder), bits(quotient)))
