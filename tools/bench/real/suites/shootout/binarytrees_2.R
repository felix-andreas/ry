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
        return(list(item,
            tree(2L * item - 1L, depth - 1L),
            tree(2L * item, depth - 1L)))
    }

    check <- function(tree)
        if(is.na(tree[[2]][[1]])) tree[[1]] else tree[[1]] + check(tree[[2]]) - check(tree[[3]])

    binarytrees_2 <- function(args) {
        n = if (length(args)) as.integer(args[[1]]) else 10L

        min_depth <- 4
        max_depth <- max(min_depth + 2, n)
        stretch_depth <- max_depth + 1

        cat(sep="", "stretch tree of depth ", stretch_depth, "\t check: ",
            check(tree(0, stretch_depth)), "\n")

        long_lived_tree <- tree(0, max_depth)

        for (depth in seq(min_depth, max_depth, 2)) {
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

    binarytrees_2(n)
}

# ---------------------------------------------------------------------------
# Driver added for ry's `just bench-real --public` (not part of the upstream
# file). It runs `execute` once at a fixed size and prints one line:
#   <name> <checksum of printed output and result> <seconds>
# Run it from this directory: upstream code reads sibling files by relative path.
local({
  started <- proc.time()[[3]]
  output <- capture.output(value <- execute(11))
  seconds <- proc.time()[[3]] - started
  text <- c(output, capture.output(str(value, vec.len = 3, digits.d = 10, give.attr = FALSE)))
  bytes <- utf8ToInt(paste(text, collapse = "\n"))
  checksum <- sprintf("%d:%.0f", length(bytes), sum(as.numeric(bytes) * (seq_along(bytes) %% 251 + 1)))
  cat(sprintf("binarytrees_2 %s %.4f\n", checksum, seconds))
})
