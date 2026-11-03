# Recursion over a tree: build an expression tree of sums, maxima and negations, then evaluate it.
type Node = :Num(value: double) | :Neg(inner: Node) | :Add(left: Node, right: Node) | :Max(left: Node, right: Node)

build <- function(depth: integer, seed: integer) -> Node {
  if (depth == 0L) return(@Node(:Num(value = seed %% 10L + 1)))
  left <- build(depth - 1L, (seed * 7L) %% 1009L + 1L)
  if (seed %% 5L == 0L) return(@Node(:Neg(inner = left)))
  right <- build(depth - 1L, (seed * 13L) %% 1013L + 3L)
  if (seed %% 2L == 0L) @Node(:Add(left = left, right = right)) else @Node(:Max(left = left, right = right))
}
evaluate <- function(node: Node) -> double {
  match (node) {
    :Num(value = v) => v
    :Neg(inner = e) => -evaluate(e)
    :Add(left = a, right = b) => evaluate(a) + evaluate(b)
    :Max(left = a, right = b) => max(evaluate(a), evaluate(b))
  }
}
tree <- build(21L, 5L)
measure("expr_tree", function() evaluate(tree) + evaluate(build(14L, 8L)))
