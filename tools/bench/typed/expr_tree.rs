// Builds an expression tree of sums, maxima and negations as a boxed enum, then evaluates it.
mod harness;

use harness::measure;
use std::hint::black_box;

enum Node {
    Num(f64),
    Neg(Box<Node>),
    Add(Box<Node>, Box<Node>),
    Max(Box<Node>, Box<Node>),
}

fn build(depth: i64, seed: i64) -> Node {
    if depth == 0 {
        return Node::Num((seed % 10 + 1) as f64);
    }
    let left = Box::new(build(depth - 1, (seed * 7) % 1009 + 1));
    if seed % 5 == 0 {
        return Node::Neg(left);
    }
    let right = Box::new(build(depth - 1, (seed * 13) % 1013 + 3));
    if seed % 2 == 0 { Node::Add(left, right) } else { Node::Max(left, right) }
}

fn evaluate(node: &Node) -> f64 {
    match node {
        Node::Num(value) => *value,
        Node::Neg(inner) => -evaluate(inner),
        Node::Add(left, right) => evaluate(left) + evaluate(right),
        Node::Max(left, right) => evaluate(left).max(evaluate(right)),
    }
}

fn main() {
    let tree = build(21, 5);
    measure("expr_tree", || evaluate(black_box(&tree)) + evaluate(&build(black_box(14), black_box(8))));
}
