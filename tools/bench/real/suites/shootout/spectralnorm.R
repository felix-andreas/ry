# ------------------------------------------------------------------
# The Computer Language Shootout
# http://shootout.alioth.debian.org/
#
# Contributed by Leo Osvald
# ------------------------------------------------------------------

execute <- function(n=1200) {
    spectralnorm <- function(args) {
        n = if (length(args)) as.integer(args[[1]]) else 100L
        options(digits=10)

        eval_A <- function(i, j) 1 / ((i + j) * (i + j + 1) / 2 + i + 1)
        eval_A_times_u <- function(u) {
            ret <- double(n)
            for (i in 0:n1) {
                eval_A_col <- double(n)
                for (j in 0:n1)
          eval_A_col[[j + 1]] <- eval_A(i, j)
                ret[[i + 1]] <- u %*% eval_A_col
            }
            return(ret)
        }
        eval_At_times_u <- function(u) {
            ret <- double(n)
            for (i in 0:n1) {
                eval_At_col <- double(n)
                for (j in 0:n1)
          eval_At_col[[j + 1]] <- eval_A(j, i)
                ret[[i + 1]] <- u %*% eval_At_col
            }
            return(ret)
        }
        eval_AtA_times_u <- function(u) eval_At_times_u(eval_A_times_u(u))

        n1 <- n - 1
        u <- rep(1, n)
        v <- rep(0, n)
#   was 10
        for (itr in seq(1)) {
            v <- eval_AtA_times_u(u)
            u <- eval_AtA_times_u(v)
        }
#    
#    cat(sqrt(sum(u * v) / sum(v * v)), "\n")
#
      
#    sqrt(sum(u * v) / sum(v * v))    
        u
    }

    spectralnorm(n)
}

# ---------------------------------------------------------------------------
# Driver added for ry's `just bench-real --public` (not part of the upstream
# file). It runs `execute` once at a fixed size and prints one line:
#   <name> <checksum of printed output and result> <seconds>
# Run it from this directory: upstream code reads sibling files by relative path.
# The guard skips the driver when another program loads this file for `execute`.
if (!exists(".ry_bench_loading")) local({
  started <- proc.time()[[3]]
  output <- capture.output(value <- execute(800))
  seconds <- proc.time()[[3]] - started
  text <- c(output, capture.output(str(value, vec.len = 3, digits.d = 10, give.attr = FALSE)))
  bytes <- utf8ToInt(paste(text, collapse = "\n"))
  checksum <- sprintf("%d:%.0f", length(bytes), sum(as.numeric(bytes) * (seq_along(bytes) %% 251 + 1)))
  cat(sprintf("spectralnorm %s %.4f\n", checksum, seconds))
})
