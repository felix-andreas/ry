# ------------------------------------------------------------------
# The Computer Language Shootout
# http://shootout.alioth.debian.org/
#
# Contributed by Leo Osvald
# ------------------------------------------------------------------

execute <- function(n=200) {

    lim <- 2
    iter <- 50

    mandelbrot_naive_ascii <- function(args) {
        n = if (length(args)) as.integer(args[[1]]) else 200L
        cat("P4\n")
        cat(n, n, "\n")
        for (y in 0:(n-1)) {
            bits <- 0L
            x <- 0L
            while (x < n) {
                c <- 2 * x / n - 1.5 + 1i * (2 * y / n - 1)
                z <- 0+0i
                i <- 0L
                while (i < iter && abs(z) <= lim) {
                    z <- z * z + c
                    i <- i + 1L
                }
                bits <- 2L * bits + as.integer(abs(z) <= lim)
                if ((x <- x + 1L) %% 8L == 0) {
                    cat(as.raw(bits), "\n")
                    bits <- 0L
                }
            }
            xmod <- x %% 8L
            if (xmod)
                cat(as.raw(bits * as.integer(2^(8L - xmod))), "\n")
        }
    }

    mandelbrot_naive_ascii(n)
}

# ---------------------------------------------------------------------------
# Driver added for ry's `just bench-real --public` (not part of the upstream
# file). It runs `execute` once at a fixed size and prints one line:
#   <name> <checksum of printed output and result> <seconds>
# Run it from this directory: upstream code reads sibling files by relative path.
# The guard skips the driver when another program loads this file for `execute`.
if (!exists(".ry_bench_loading")) local({
  started <- proc.time()[[3]]
  output <- capture.output(value <- execute(300))
  seconds <- proc.time()[[3]] - started
  text <- c(output, capture.output(str(value, vec.len = 3, digits.d = 10, give.attr = FALSE)))
  bytes <- utf8ToInt(paste(text, collapse = "\n"))
  checksum <- sprintf("%d:%.0f", length(bytes), sum(as.numeric(bytes) * (seq_along(bytes) %% 251 + 1)))
  cat(sprintf("mandelbrot_naive_ascii %s %.4f\n", checksum, seconds))
})
