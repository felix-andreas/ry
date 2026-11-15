# The tree is a tag union instead of nested lists, and the random seed and the node count are
# threaded through the calls instead of updated with <<-. The R program passes a `random`
# argument that it never reads, so it is left out.
type Tree = :Leaf(I64) | :Node(List<Tree>)

execute <- function() -> F64 {
  built <- build_tree(10, 74755, 0)
  Num.to_f64(built$count)
}

build_tree <- function(depth: I64, seed: I64, count: I64) -> Record{tree: Tree, seed: I64, count: I64} {
  count <- count + 1
  if (depth == 1) {
    seed <- Num.bitwise_and(seed * 1309 + 13849, 65535)
    return((tree = @Tree(:Leaf(seed %% 10 + 1)), seed = seed, count = count))
  }
  children <- []
  for (i in List.range(1, 4)) {
    built <- build_tree(depth - 1, seed, count)
    children <- List.append(children, built$tree)
    seed <- built$seed
    count <- built$count
  }
  (tree = @Tree(:Node(children)), seed = seed, count = count)
}

# ---------------------------------------------------------------------------
# Driver: the same as storage.R's, except that the R value the checksum reads is
# built from the typed result after the clock stops.
local({
  started <- proc.time()[[3]]
  output <- capture.output(result <- execute())
  seconds <- proc.time()[[3]] - started
  value <- result
  text <- c(output, capture.output(str(value, vec.len = 3, digits.d = 10, give.attr = FALSE)))
  bytes <- utf8ToInt(paste(text, collapse = "\n"))
  checksum <- sprintf("%d:%.0f", length(bytes), sum(as.numeric(bytes) * (seq_along(bytes) %% 251 + 1)))
  cat(sprintf("storage %s %.4f\n", checksum, seconds))
})
