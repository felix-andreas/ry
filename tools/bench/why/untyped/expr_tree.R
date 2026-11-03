# Recursion over a tree: build an expression tree of sums, maxima and negations, then evaluate it.
build <- function(depth, seed) {
  if (depth == 0L) return(list(kind = "num", value = seed %% 10L + 1))
  left <- build(depth - 1L, (seed * 7L) %% 1009L + 1L)
  if (seed %% 5L == 0L) return(list(kind = "neg", inner = left))
  right <- build(depth - 1L, (seed * 13L) %% 1013L + 3L)
  if (seed %% 2L == 0L) list(kind = "add", left = left, right = right) else list(kind = "max", left = left, right = right)
}
evaluate <- function(node) {
  switch(node$kind,
    num = node$value,
    neg = -evaluate(node$inner),
    add = evaluate(node$left) + evaluate(node$right),
    max = max(evaluate(node$left), evaluate(node$right))
  )
}
tree <- build(21L, 5L)
measure("expr_tree", function() evaluate(tree) + evaluate(build(14L, 8L)))
