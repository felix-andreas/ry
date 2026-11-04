# ------------------------------------------------------------------
# The Computer Language Shootout
# http://shootout.alioth.debian.org/
#
# Contributed by Leo Osvald
# ------------------------------------------------------------------

execute <- function(n=250) {
    spectralnorm_alt_3 <- function(args) {
        n = if (length(args)) as.integer(args[[1]]) else 100L
        options(digits=10)

        eval_A <- function(i, j) eval_A_cache[[i, j]]
        eval_A_times_u <- function(u) {
            #    eval_A_mat <- outer(seq(n), seq(n), FUN=eval_A)
            eval_A_mat <- matrix(0, n, n)
            for (i in 1:n)
                for (j in 1:n)
                    eval_A_mat[[i, j]] <- eval_A(i, j)
            return(u %*% t(eval_A_mat))
        }
        eval_At_times_u <- function(u) {
            # eval_At_mat <- t(outer(seq(n), seq(n), FUN=eval_A))
            eval_At_mat <- matrix(0, n, n)
            for (i in 1:n)
                for (j in 1:n)
                    eval_At_mat[[i, j]] <- eval_A(i, j)
            return(u %*% eval_At_mat)
        }
        eval_AtA_times_u <- function(u) eval_At_times_u(eval_A_times_u(u))
        eval_A_cache <- matrix(0, n, n)
            for (i in 1:n)
                for (j in 1:n)
                    eval_A_cache[[i, j]] <- 1/((i + j - 2) * (i + j - 1) / 2 + i)
        u <- rep(1, n)
        v <- rep(0, n)
        for (itr in seq(10)) {
            v <- eval_AtA_times_u(u)
            u <- eval_AtA_times_u(v)
        }

        cat(sqrt(sum(u * v) / sum(v * v)), "\n")
    }

    spectralnorm_alt_3(n)
}

# ---------------------------------------------------------------------------
# Driver added for ry's `just bench-real --public` (not part of the upstream
# file). It runs `execute` once at a fixed size and prints one line:
#   <name> <checksum of printed output and result> <seconds>
# Run it from this directory: upstream code reads sibling files by relative path.
# The guard skips the driver when another program loads this file for `execute`.
if (!exists(".ry_bench_loading")) local({
  started <- proc.time()[[3]]
  output <- capture.output(value <- execute(250))
  seconds <- proc.time()[[3]] - started
  text <- c(output, capture.output(str(value, vec.len = 3, digits.d = 10, give.attr = FALSE)))
  bytes <- utf8ToInt(paste(text, collapse = "\n"))
  checksum <- sprintf("%d:%.0f", length(bytes), sum(as.numeric(bytes) * (seq_along(bytes) %% 251 + 1)))
  cat(sprintf("spectralnorm_alt_3 %s %.4f\n", checksum, seconds))
})
