# Recursion over a tree: build an expression tree of sums, maxima and negations, then evaluate it.
type Node = :Num(value: F64) | :Neg(inner: Node) | :Add(left: Node, right: Node) | :Max(left: Node, right: Node)

build <- function(depth: I64, seed: I64) -> Node {
  if (depth == 0) return(@Node(:Num(value = Num.to_f64(seed %% 10 + 1))))
  left <- build(depth - 1, (seed * 7) %% 1009 + 1)
  if (seed %% 5 == 0) return(@Node(:Neg(inner = left)))
  right <- build(depth - 1, (seed * 13) %% 1013 + 3)
  if (seed %% 2 == 0) @Node(:Add(left = left, right = right)) else @Node(:Max(left = left, right = right))
}
evaluate <- function(node: Node) -> F64 {
  match (node) {
    :Num(value = v) => v
    :Neg(inner = e) => -evaluate(e)
    :Add(left = a, right = b) => evaluate(a) + evaluate(b)
    :Max(left = a, right = b) => Num.max(evaluate(a), evaluate(b))
  }
}
tree <- build(21, 5)
measure("expr_tree", function() evaluate(tree) + evaluate(build(14, 8)))
