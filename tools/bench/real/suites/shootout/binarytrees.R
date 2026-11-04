# ------------------------------------------------------------------
# The Computer Language Shootout
# http://shootout.alioth.debian.org/
#
# Contributed by Leo Osvald
# ------------------------------------------------------------------

execute <- function(n=9) {
    tree <- function(item, depth) {
        if (depth == 0L)
            return(c(item, NA, NA))
        # it is ridiculous that this doesn't help
        next_depth <- depth - 1L
        right_item <- 2L * item
        left_item <- right_item - 1L
        return(list(item,
                    tree(left_item, next_depth),
                    tree(right_item, next_depth)))
    }

    check <- function(tree)
        if(is.na(tree[[2]][[1]])) tree[[1]] else tree[[1]] + check(tree[[2]]) - check(tree[[3]])

    binarytrees <- function(args) {
        n = if (length(args)) as.integer(args[[1]]) else 13L

        min_depth <- 4L
        max_depth <- max(min_depth + 2L, n)
        stretch_depth <- max_depth + 1L

        cat(sep="", "stretch tree of depth ", stretch_depth, "\t check: ",
            check(tree(0L, stretch_depth)), "\n")

        long_lived_tree <- tree(0L, max_depth)

        for (depth in seq(min_depth, max_depth, 2L)) {
            iterations <- as.integer(2^(max_depth - depth + min_depth))
            check_sum <- sum(sapply(
                    1:iterations,
        function(i) check(tree(i, depth)) + check(tree(-i, depth))))
            cat(sep="", iterations * 2L, "\t trees of depth ", depth, "\t check: ",
                check_sum, "\n")
        }

        cat(sep="", "long lived tree of depth ", max_depth, "\t check: ", 
            check(long_lived_tree), "\n")
    }

    binarytrees(n)
}

# ---------------------------------------------------------------------------
# Driver added for ry's `just bench-real --public` (not part of the upstream
# file). It runs `execute` once at a fixed size and prints one line:
#   <name> <checksum of printed output and result> <seconds>
# Run it from this directory: upstream code reads sibling files by relative path.
# The guard skips the driver when another program loads this file for `execute`.
if (!exists(".ry_bench_loading")) local({
  started <- proc.time()[[3]]
  output <- capture.output(value <- execute(12))
  seconds <- proc.time()[[3]] - started
  text <- c(output, capture.output(str(value, vec.len = 3, digits.d = 10, give.attr = FALSE)))
  bytes <- utf8ToInt(paste(text, collapse = "\n"))
  checksum <- sprintf("%d:%.0f", length(bytes), sum(as.numeric(bytes) * (seq_along(bytes) %% 251 + 1)))
  cat(sprintf("binarytrees %s %.4f\n", checksum, seconds))
})
