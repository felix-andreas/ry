# ------------------------------------------------------------------
# The Computer Language Shootout
# http://shootout.alioth.debian.org/
#
# Contributed by Leo Osvald
# ------------------------------------------------------------------

execute <- function(n=150) {
    spectralnorm_naive <- function(args) {
        n = if (length(args)) as.integer(args[[1]]) else 100L
        options(digits=10)

        eval_A <- function(i, j) 1 / ((i + j) * (i + j + 1) / 2 + i + 1)
        # TODO: get rid of this hack
        eval_A_2 <- function(i, j) 1 / ((i + j) * (i + j + 1) / 2 + i + 1)

        eval_A_times_u <- function(u) {
            ret <- rep(0, n)
            for (i in 1:n)
          for (j in 0:n1)
                    ret[[i]] <- ret[[i]] + u[[j + 1]] * eval_A(i - 1, j)
            return(ret)
        }
        eval_At_times_u <- function(u) {
            ret <- rep(0, n)
            for (i in 1:n)
          for (j in 0:n1)
                    ret[[i]] <- ret[[i]] + u[[j + 1]] * eval_A_2(j, i - 1)
            return(ret)
        }
        eval_AtA_times_u <- function(u) eval_At_times_u(eval_A_times_u(u))

        n1 <- n - 1
        u <- rep(1, n)
        v <- rep(0, n)
        for (itr in seq(10)) {
            v <- eval_AtA_times_u(u)
            u <- eval_AtA_times_u(v)
        }

        cat(sqrt(sum(u * v) / sum(v * v)), "\n")
    }

    spectralnorm_naive(n)
}

# ---------------------------------------------------------------------------
# Driver added for ry's `just bench-real --public` (not part of the upstream
# file). It runs `execute` once at a fixed size and prints one line:
#   <name> <checksum of printed output and result> <seconds>
# Run it from this directory: upstream code reads sibling files by relative path.
local({
  started <- proc.time()[[3]]
  output <- capture.output(value <- execute(250))
  seconds <- proc.time()[[3]] - started
  text <- c(output, capture.output(str(value, vec.len = 3, digits.d = 10, give.attr = FALSE)))
  bytes <- utf8ToInt(paste(text, collapse = "\n"))
  checksum <- sprintf("%d:%.0f", length(bytes), sum(as.numeric(bytes) * (seq_along(bytes) %% 251 + 1)))
  cat(sprintf("spectralnorm_naive %s %.4f\n", checksum, seconds))
})
