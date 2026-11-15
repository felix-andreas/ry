type Tree = :Leaf(I64) | :Node(item: I64, left: Tree, right: Tree)

tree <- function(item: I64, depth: I64) -> Tree {
  if (depth == 0)
    return(@Tree(:Leaf(item)))
  @Tree(:Node(item = item, left = tree(2 * item - 1, depth - 1), right = tree(2 * item, depth - 1)))
}

check <- function(tree: Tree) -> I64 {
  match (tree) {
    :Leaf(item) => item
    :Node(item = item, left = left, right = right) => item + check(left) - check(right)
  }
}

execute <- function(n: I64) -> List<Str> {
  min_depth: I64 <- 4
  max_depth <- Num.max(min_depth + 2, n)
  stretch_depth <- max_depth + 1

  lines <- [f"stretch tree of depth {stretch_depth}\t check: {check(tree(0, stretch_depth))}\n"]

  long_lived_tree <- tree(0, max_depth)

  depth <- min_depth
  while (depth <= max_depth) {
    iterations <- Num.pow_int(2, max_depth - depth + min_depth)
    check_sum <- 0
    for (i in List.range(1, iterations))
      check_sum <- check_sum + check(tree(i, depth)) + check(tree(-i, depth))
    lines <- List.append(lines, f"{iterations * 2}\t trees of depth {depth}\t check {check_sum}\n")
    depth <- depth + 2
  }

  List.append(lines, f"long lived tree of depth {max_depth}\t check: {check(long_lived_tree)}\n")
}

# ---------------------------------------------------------------------------
# Driver: the same as binarytrees_naive.R's, except that the typed program returns its lines
# and the driver prints them inside the clock.
local({
  started <- proc.time()[[3]]
  output <- capture.output(cat(List.to_r(execute(12)), sep = ""))
  seconds <- proc.time()[[3]] - started
  value <- NULL
  text <- c(output, capture.output(str(value, vec.len = 3, digits.d = 10, give.attr = FALSE)))
  bytes <- utf8ToInt(paste(text, collapse = "\n"))
  checksum <- sprintf("%d:%.0f", length(bytes), sum(as.numeric(bytes) * (seq_along(bytes) %% 251 + 1)))
  cat(sprintf("binarytrees_naive %s %.4f\n", checksum, seconds))
})
