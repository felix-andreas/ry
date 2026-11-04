# ------------------------------------------------------------------
# The Computer Language Shootout
# http://shootout.alioth.debian.org/
#
# Contributed by Leo Osvald
# ------------------------------------------------------------------

execute <- function(n=1200) {
    spectralnorm_alt_2 <- function(args) {
        n = if (length(args)) as.integer(args[[1]]) else 100L
        options(digits=10)

        n = if (length(args)) as.integer(args[[1]]) else 100L

        eval_A <- function(i, j) 1 / ((i + j - 2) * (i + j - 1) / 2 + i)
        eval_A_times_u <- function(u)
            u %*% outer(seq(n), seq(n), FUN=eval_A)
        eval_At_times_u <- function(u)
            u %*% t(outer(seq(n), seq(n), FUN=eval_A))
        eval_AtA_times_u <- function(u)
        eval_At_times_u(eval_A_times_u(u))

        u <- rep(1, n)
        v <- rep(0, n)
        for (itr in seq(10)) {
            v <- eval_AtA_times_u(u)
            u <- eval_AtA_times_u(v)
        }
        cat(sqrt(sum(u * v) / sum(v * v)), "\n")
    }

    spectralnorm_alt_2(n)
}

# ---------------------------------------------------------------------------
# Driver added for ry's `just bench-real --public` (not part of the upstream
# file). It runs `execute` once at a fixed size and prints one line:
#   <name> <checksum of printed output and result> <seconds>
# Run it from this directory: upstream code reads sibling files by relative path.
local({
  started <- proc.time()[[3]]
  output <- capture.output(value <- execute(1200))
  seconds <- proc.time()[[3]] - started
  text <- c(output, capture.output(str(value, vec.len = 3, digits.d = 10, give.attr = FALSE)))
  bytes <- utf8ToInt(paste(text, collapse = "\n"))
  checksum <- sprintf("%d:%.0f", length(bytes), sum(as.numeric(bytes) * (seq_along(bytes) %% 251 + 1)))
  cat(sprintf("spectralnorm_alt_2 %s %.4f\n", checksum, seconds))
})
