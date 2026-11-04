source('random.r')

count <- 0

execute <- function(size) {
    buildTreeDepth <- function(depth, random) {
        count <<- count + 1
        if (depth == 1) {
            return (c(nextRandom() %% 10 + 1))
        } else {
            array <- vector("list", length = 4)
            for (i in 1:4) {
                array[[i]] <- buildTreeDepth(depth - 1, random)
            }
            return (array)
        }
    }

    count <<- 0
    resetSeed()
    buildTreeDepth(10, nextRandom())
    return (count) 
}


# ---------------------------------------------------------------------------
# Driver added for ry's `just bench-real --public` (not part of the upstream
# file). It runs `execute` once at a fixed size and prints one line:
#   <name> <checksum of printed output and result> <seconds>
# Run it from this directory: upstream code reads sibling files by relative path.
local({
  started <- proc.time()[[3]]
  output <- capture.output(value <- execute(NULL))
  seconds <- proc.time()[[3]] - started
  text <- c(output, capture.output(str(value, vec.len = 3, digits.d = 10, give.attr = FALSE)))
  bytes <- utf8ToInt(paste(text, collapse = "\n"))
  checksum <- sprintf("%d:%.0f", length(bytes), sum(as.numeric(bytes) * (seq_along(bytes) %% 251 + 1)))
  cat(sprintf("storage %s %.4f\n", checksum, seconds))
})
