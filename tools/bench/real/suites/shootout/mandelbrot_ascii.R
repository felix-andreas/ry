# ------------------------------------------------------------------
# The Computer Language Shootout
# http://shootout.alioth.debian.org/
#
# Contributed by Leo Osvald
# ------------------------------------------------------------------

execute <- function(n=300) {
    lim <- 2
    iter <- 50

    mandelbrot_ascii <- function(args) {
        n = if (length(args)) as.integer(args[[1]]) else 200L
        n_mod8 = n %% 8L
        pads <- if (n_mod8) rep.int(0, 8L - n_mod8) else integer(0)
        p <- rep(as.integer(rep.int(2, 8) ^ (7:0)), length.out=n)

        cat("P4\n")
        cat(n, n, "\n")
        for (y in 0:(n-1)) {
            c <- 2 * 0:(n-1) / n - 1.5 + 1i * (2 * y / n - 1)
            z <- rep(0+0i, n)
            i <- 0L
            while (i < iter) {  # faster than for loop
                z <- z * z + c
                i <- i + 1L
            }
            bits <- as.integer(abs(z) <= lim)
            bytes <- as.raw(colSums(matrix(c(bits * p, pads), 8L)))
      cat(bytes,"\n")
        }
    }

    mandelbrot_ascii(n)
}

# ---------------------------------------------------------------------------
# Driver added for ry's `just bench-real --public` (not part of the upstream
# file). It runs `execute` once at a fixed size and prints one line:
#   <name> <checksum of printed output and result> <seconds>
# Run it from this directory: upstream code reads sibling files by relative path.
local({
  started <- proc.time()[[3]]
  output <- capture.output(value <- execute(1300))
  seconds <- proc.time()[[3]] - started
  text <- c(output, capture.output(str(value, vec.len = 3, digits.d = 10, give.attr = FALSE)))
  bytes <- utf8ToInt(paste(text, collapse = "\n"))
  checksum <- sprintf("%d:%.0f", length(bytes), sum(as.numeric(bytes) * (seq_along(bytes) %% 251 + 1)))
  cat(sprintf("mandelbrot_ascii %s %.4f\n", checksum, seconds))
})
