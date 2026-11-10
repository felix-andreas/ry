//! The element loops behind R's vector builtins, and the entry points compiled code calls.
//!
//! A kernel works on slices: it reads its operands as [`Reals`] or [`Integers`] (a vector, a
//! scalar, or the output buffer itself when the result reuses an operand's storage) and writes
//! every element of the output slice. Kernels know nothing of `SEXP` headers, attributes or
//! dispatch, so a builtin, the bytecode's arithmetic instruction and a fused element-wise tree
//! share them, and compiled code can call them on its own buffers. Each kernel is compiled
//! twice, for AVX2 and for the SSE2 baseline, and picks the wider build at run time.
//!
//! The results match GNU R on x86-64 bit for bit, NaN payloads included: an operation on two
//! NaNs returns the first operand's payload, which is how `NA + NaN` stays `NA` and `NaN + NA`
//! stays `NaN`. Floating-point addition and multiplication commute as far as the compiler is
//! concerned, so it may swap their operands, and the kernels pick the payload explicitly.

use {
    crate::{
        arith::{self, Math, Op},
        bytecode::Arith,
        conditions,
        long_double::{self, LongDouble},
        unwind::R,
        values,
    },
    heap::object::{NA_INTEGER, NA_LOGICAL, NA_REAL, Sexp, SexpType, is_na_real},
    std::cmp::Ordering,
};

/// A read-only numeric operand: doubles, integers or logicals read as doubles, or one value
/// that stands beside every element. A vector shorter than the output is recycled.
#[derive(Clone, Copy)]
pub enum Reals<'a> {
    Doubles(&'a [f64]),
    Integers(&'a [i32]),
    Scalar(f64),
}

/// A read-only integer or logical operand.
#[derive(Clone, Copy)]
pub enum Integers<'a> {
    Values(&'a [i32]),
    Scalar(i32),
}

/// Which operand an in-place kernel finds in its output buffer.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Left,
    Right,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Comparison {
    Equal,
    NotEqual,
    Less,
    Greater,
    LessEqual,
    GreaterEqual,
}

impl Comparison {
    pub fn holds(self, ordering: Ordering) -> bool {
        match self {
            Comparison::Equal => ordering == Ordering::Equal,
            Comparison::NotEqual => ordering != Ordering::Equal,
            Comparison::Less => ordering == Ordering::Less,
            Comparison::Greater => ordering == Ordering::Greater,
            Comparison::LessEqual => ordering != Ordering::Greater,
            Comparison::GreaterEqual => ordering != Ordering::Less,
        }
    }
}

/// The result of an element-wise operation on two plain vectors, or `None` when the operation
/// needs the builtin: an operand with attributes, an object, ALTREP, an Array, a type other than
/// logical, integer and double, a zero length, or lengths that recycle unevenly. Bit `0` of
/// `reuse` marks the left operand and bit `1` the right as a temporary nothing else holds (the
/// compiler sets them, see [`crate::bytecode`]); such an operand with no counted reference, the
/// result's type and the result's length becomes the result.
pub unsafe fn binary(
    call: Sexp,
    operator: Arith,
    left: Sexp,
    right: Sexp,
    reuse: u32,
) -> R<Option<Sexp>> {
    if !plain(left) || !plain(right) {
        return Ok(None);
    }
    let (left_length, right_length) = (left.length() as usize, right.length() as usize);
    let count = if left_length == right_length || right_length == 1 {
        left_length
    } else if left_length == 1 {
        right_length
    } else {
        return Ok(None);
    };
    if count == 0 {
        return Ok(None);
    }
    let integral = |value: Sexp| value.kind() != SexpType::Real;
    let both_integral = integral(left) && integral(right);
    if let Some(comparison) = comparison_of(operator) {
        let result = values::alloc_uninit(SexpType::Logical, count)?;
        let output = values::ints_mut(result);
        if both_integral {
            compare_integers(comparison, integers(left), integers(right), output);
        } else {
            compare_reals(comparison, reals(left), reals(right), output);
        }
        if count == 1 {
            return Ok(Some(values::scalar_logical(output[0])?));
        }
        return Ok(Some(result));
    }
    let op = arithmetic_of(operator);
    let kind = if both_integral && matches!(op, Op::Add | Op::Subtract | Op::Multiply) {
        SexpType::Integer
    } else {
        SexpType::Real
    };
    // A length-one result is left alone: compiled code may hold a boxed scalar in more places
    // than one register, and a fresh scalar costs little.
    let reusable = |value: Sexp, bit: u32| {
        reuse & bit != 0
            && count > 1
            && left != right
            && value.kind() == kind
            && value.length() as usize == count
            && value.refcount() == 0
    };
    let (result, side) = if reusable(left, 1) {
        (left, Some(Side::Left))
    } else if reusable(right, 2) {
        (right, Some(Side::Right))
    } else {
        (values::alloc_uninit(kind, count)?, None)
    };
    if kind == SexpType::Real {
        let output = values::reals_mut(result);
        let has_integer = left.kind() == SexpType::Integer || right.kind() == SexpType::Integer;
        // Swapping the operands of `+` or `*` changes nothing but which NaN payload wins.
        let swap = payload_from_right(op, has_integer, left_length, right_length);
        let (left, right, side) = match (swap, side) {
            (false, side) => (left, right, side),
            (true, None) => (right, left, None),
            (true, Some(Side::Left)) => (right, left, Some(Side::Right)),
            (true, Some(Side::Right)) => (right, left, Some(Side::Left)),
        };
        match side {
            None => real_arithmetic(op, reals(left), reals(right), output),
            Some(Side::Left) => real_arithmetic_into(op, Side::Left, reals(right), output),
            Some(Side::Right) => real_arithmetic_into(op, Side::Right, reals(left), output),
        }
        return Ok(Some(result));
    }
    let output = values::ints_mut(result);
    let overflowed = match side {
        None => integer_arithmetic(op, integers(left), integers(right), output),
        Some(Side::Left) => integer_arithmetic_into(op, Side::Left, integers(right), output),
        Some(Side::Right) => integer_arithmetic_into(op, Side::Right, integers(left), output),
    };
    if overflowed {
        conditions::warning_call(call, "NAs produced by integer overflow")?;
    }
    Ok(Some(result))
}

/// Whether GNU R's `x + y` or `x * y` gives a NaN element of `y` before one of `x`, which is
/// only observable when both are NaN: `NA_real_ + c(NaN, 1)` is `NaN` but `c(NA, 1) + NaN` is
/// `NA`. GNU R's loops over two double vectors (logicals become doubles first) are compiled with
/// the operands swapped where `x` is a scalar beside a longer `y` and where both recycle, while
/// the loops that mix in an integer vector keep them in order.
pub fn payload_from_right(
    op: Op,
    has_integer: bool,
    left_length: usize,
    right_length: usize,
) -> bool {
    matches!(op, Op::Add | Op::Multiply)
        && !has_integer
        && left_length != right_length
        && right_length != 1
}

/// Whether a kernel may read `value` directly: a logical, integer or double vector with no
/// attributes that is neither ALTREP nor an Array.
pub unsafe fn plain(value: Sexp) -> bool {
    matches!(
        value.kind(),
        SexpType::Logical | SexpType::Integer | SexpType::Real
    ) && value.attrib() == values::nil()
        && !value.is_altrep()
        && !value.is_array()
}

/// A plain vector's elements as a [`Reals`] operand.
pub unsafe fn reals<'a>(value: Sexp) -> Reals<'a> {
    match value.kind() {
        SexpType::Real => Reals::Doubles(values::reals(value)),
        _ => Reals::Integers(values::ints(value)),
    }
}

/// A plain integer or logical vector's elements as an [`Integers`] operand.
pub unsafe fn integers<'a>(value: Sexp) -> Integers<'a> {
    Integers::Values(values::ints(value))
}

pub fn comparison_of(operator: Arith) -> Option<Comparison> {
    Some(match operator {
        Arith::Equal => Comparison::Equal,
        Arith::NotEqual => Comparison::NotEqual,
        Arith::Less => Comparison::Less,
        Arith::Greater => Comparison::Greater,
        Arith::LessEqual => Comparison::LessEqual,
        Arith::GreaterEqual => Comparison::GreaterEqual,
        _ => return None,
    })
}

/// The arithmetic operator of a non-comparison [`Arith`].
pub fn arithmetic_of(operator: Arith) -> Op {
    match operator {
        Arith::Add => Op::Add,
        Arith::Subtract => Op::Subtract,
        Arith::Multiply => Op::Multiply,
        Arith::Divide => Op::Divide,
        _ => Op::Power,
    }
}

/// The most nodes, leaves included, a fused tree may have.
pub const MAX_NODES: usize = 32;

/// How many elements a fused tree computes at a time: each intermediate result lives in a
/// buffer of this many elements, small enough to stay in the first-level cache.
const CHUNK: usize = 512;

/// One node of a fused element-wise tree. Nodes come in post-order, so a node's inputs are
/// earlier nodes and the last node is the root.
#[derive(Clone, Copy)]
pub enum Node {
    /// Leaf `index` of the tree's leaves.
    Leaf(usize),
    Binary(Arith, usize, usize),
    /// `&` when `is_and`, otherwise `|`.
    Logic {
        is_and: bool,
        left: usize,
        right: usize,
    },
    Negate(usize),
    Not(usize),
    Math(Math, usize),
}

/// A reduction applied to the root of a fused tree, whose vector is then never stored.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Reduction {
    Sum,
    Any,
    All,
}

/// The element type of a node's result.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Element {
    Logical,
    Integer,
    Double,
}

/// What a fused tree computed: the value, and each node's element type and whether its
/// result had one element, which is what the instructions the tree replaces would have
/// recorded as type feedback.
pub struct Fused {
    pub value: Sexp,
    pub elements: [Element; MAX_NODES],
    pub scalar: [bool; MAX_NODES],
}

/// Evaluates the element-wise tree `nodes` over the plain vectors `leaves`, `CHUNK` elements at
/// a time, so no intermediate result is ever a whole vector: `exp(a * b + c)` reads each
/// element of `a`, `b` and `c` once and writes the result once, and `sum(x > 0)` writes
/// nothing but its total. `None` sends the expression to its unfused code, which then gives
/// the result or the error or warning: a leaf that is not plain, lengths that differ other
/// than by a scalar, fewer than two elements, an operand type the fused operators do not
/// cover, and any result that would warn (integer overflow, NaN from a math function) or
/// whose rounding the unfused builtin handles specially (a non-finite double total).
pub unsafe fn fused(
    nodes: &[Node],
    leaves: &[Sexp],
    reduction: Option<Reduction>,
) -> R<Option<Fused>> {
    let Some(root) = nodes.len().checked_sub(1) else {
        return Ok(None);
    };
    if nodes.len() > MAX_NODES || leaves.iter().any(|&leaf| !plain(leaf)) {
        return Ok(None);
    }
    let count = leaves
        .iter()
        .map(|&leaf| leaf.length() as usize)
        .max()
        .unwrap_or(0);
    if count < 2
        || leaves.iter().any(|&leaf| {
            let length = leaf.length() as usize;
            length != 1 && length != count
        })
    {
        return Ok(None);
    }
    let mut elements = [Element::Double; MAX_NODES];
    let mut scalar = [false; MAX_NODES];
    for (index, node) in nodes.iter().enumerate() {
        let integral = |input: usize| elements[input] != Element::Double;
        let logical = |input: usize| elements[input] == Element::Logical;
        let element = match *node {
            Node::Leaf(leaf) => Some(match leaves[leaf].kind() {
                SexpType::Logical => Element::Logical,
                SexpType::Integer => Element::Integer,
                _ => Element::Double,
            }),
            Node::Binary(operator, _, _) if comparison_of(operator).is_some() => {
                Some(Element::Logical)
            }
            Node::Binary(Arith::Add | Arith::Subtract | Arith::Multiply, left, right)
                if integral(left) && integral(right) =>
            {
                Some(Element::Integer)
            }
            Node::Binary(..) => Some(Element::Double),
            Node::Logic { left, right, .. } => {
                (logical(left) && logical(right)).then_some(Element::Logical)
            }
            Node::Negate(input) if integral(input) => Some(Element::Integer),
            Node::Negate(_) => Some(Element::Double),
            Node::Not(input) => logical(input).then_some(Element::Logical),
            // `abs` keeps integers, which the double buffers do not.
            Node::Math(Math::Abs, input) if integral(input) => None,
            Node::Math(..) => Some(Element::Double),
        };
        let Some(element) = element else {
            return Ok(None);
        };
        elements[index] = element;
        scalar[index] = match *node {
            Node::Leaf(leaf) => leaves[leaf].length() == 1,
            Node::Binary(_, left, right) | Node::Logic { left, right, .. } => {
                scalar[left] && scalar[right]
            }
            Node::Negate(input) | Node::Not(input) | Node::Math(_, input) => scalar[input],
        };
    }
    if scalar[root]
        || matches!(reduction, Some(Reduction::Any | Reduction::All))
            && elements[root] != Element::Logical
    {
        return Ok(None);
    }

    let leaf_view = |leaf: usize, start: usize, length: usize| {
        let value = leaves[leaf];
        let range = if value.length() == 1 {
            0..1
        } else {
            start..start + length
        };
        match value.kind() {
            SexpType::Real => View::Doubles(&values::reals(value)[range]),
            _ => View::Integers(&values::ints(value)[range]),
        }
    };

    // Nodes whose inputs are all scalars run once, before the chunks.
    let mut scalar_reals = [[0.0; 1]; MAX_NODES];
    let mut scalar_integers = [[0; 1]; MAX_NODES];
    for (index, node) in nodes.iter().enumerate() {
        if !scalar[index] || matches!(node, Node::Leaf(_)) {
            continue;
        }
        let (real_done, real_rest) = scalar_reals.split_at_mut(index);
        let (integer_done, integer_rest) = scalar_integers.split_at_mut(index);
        let view = |input: usize| match nodes[input] {
            Node::Leaf(leaf) => leaf_view(leaf, 0, 1),
            _ if elements[input] == Element::Double => View::Doubles(&real_done[input]),
            _ => View::Integers(&integer_done[input]),
        };
        let output = match elements[index] {
            Element::Double => Output::Doubles(&mut real_rest[0]),
            _ => Output::Integers(&mut integer_rest[0]),
        };
        if !apply(*node, view, output) {
            return Ok(None);
        }
    }

    let result = match reduction {
        None => Some(values::alloc_uninit(
            match elements[root] {
                Element::Logical => SexpType::Logical,
                Element::Integer => SexpType::Integer,
                Element::Double => SexpType::Real,
            },
            count,
        )?),
        Some(_) => None,
    };
    // Each node computed per chunk gets its own buffer; leaves and scalars need none.
    let mut slots = [0; MAX_NODES];
    let mut buffers = 0;
    for (index, node) in nodes.iter().enumerate() {
        if !scalar[index] && !matches!(node, Node::Leaf(_)) {
            slots[index] = buffers;
            buffers += 1;
        }
    }
    let mut scratch = SCRATCH.take();
    let needed = buffers * CHUNK;
    if scratch.reals.len() < needed {
        scratch.reals.resize(needed, 0.0);
        scratch.integers.resize(needed, 0);
    }
    let outcome = chunks(
        nodes,
        &elements,
        &scalar,
        &slots,
        &scalar_reals,
        &scalar_integers,
        leaf_view,
        result,
        reduction,
        count,
        &mut scratch,
    );
    SCRATCH.set(scratch);
    let Some(total) = outcome else {
        return Ok(None);
    };
    let value = match (result, total) {
        (Some(result), _) => result,
        (None, Total::Double(total)) => values::scalar_real(total)?,
        (None, Total::Integer(total)) => values::scalar_integer(total)?,
        (None, Total::Logical(total)) => values::scalar_logical(total)?,
    };
    Ok(Some(Fused {
        value,
        elements,
        scalar,
    }))
}

/// The scratch buffers fused trees compute their chunks in, kept between evaluations so a
/// tree allocates nothing. Evaluating a tree never runs R code, so it never nests.
#[derive(Default)]
struct Scratch {
    reals: Vec<f64>,
    integers: Vec<i32>,
}

thread_local! {
    static SCRATCH: std::cell::Cell<Scratch> = std::cell::Cell::new(Scratch::default());
}

/// A reduction's result.
enum Total {
    Double(f64),
    Integer(i32),
    Logical(i32),
}

/// A node's input for one chunk: a slice of the chunk's length, or of length one for a scalar.
#[derive(Clone, Copy)]
enum View<'a> {
    Doubles(&'a [f64]),
    Integers(&'a [i32]),
}

enum Output<'a> {
    Doubles(&'a mut [f64]),
    Integers(&'a mut [i32]),
}

/// Runs the vector nodes of a fused tree chunk by chunk, into `result` or into the root's
/// reduction. `None` when a result would warn.
#[allow(clippy::too_many_arguments)]
unsafe fn chunks<'a>(
    nodes: &[Node],
    elements: &[Element; MAX_NODES],
    scalar: &[bool; MAX_NODES],
    slots: &[usize; MAX_NODES],
    scalar_reals: &[[f64; 1]; MAX_NODES],
    scalar_integers: &[[i32; 1]; MAX_NODES],
    leaf_view: impl Fn(usize, usize, usize) -> View<'a>,
    result: Option<Sexp>,
    reduction: Option<Reduction>,
    count: usize,
    scratch: &mut Scratch,
) -> Option<Total> {
    let root = nodes.len() - 1;
    // GNU R computes the whole vector before `any` or `all` looks at it, so the scan stops
    // early only when no element could have warned.
    let warns = nodes.iter().enumerate().any(|(index, node)| match node {
        Node::Math(..) => true,
        Node::Binary(operator, ..) => {
            comparison_of(*operator).is_none() && elements[index] == Element::Integer
        }
        _ => false,
    });
    let mut double_total = LongDouble::ZERO;
    let mut integer_total = 0i64;
    let mut missing = false;
    // A chunk that settles `any` or `all` while later chunks may still warn.
    let mut settled = false;
    let mut start = 0;
    while start < count {
        let length = CHUNK.min(count - start);
        for (index, node) in nodes.iter().enumerate() {
            if scalar[index] || matches!(node, Node::Leaf(_)) {
                continue;
            }
            let at = slots[index] * CHUNK;
            let (real_done, real_rest) = scratch.reals.split_at_mut(at);
            let (integer_done, integer_rest) = scratch.integers.split_at_mut(at);
            let view = |input: usize| match nodes[input] {
                Node::Leaf(leaf) => leaf_view(leaf, start, length),
                _ if scalar[input] && elements[input] == Element::Double => {
                    View::Doubles(&scalar_reals[input])
                }
                _ if scalar[input] => View::Integers(&scalar_integers[input]),
                _ if elements[input] == Element::Double => {
                    View::Doubles(&real_done[slots[input] * CHUNK..][..length])
                }
                _ => View::Integers(&integer_done[slots[input] * CHUNK..][..length]),
            };
            let output = match (index == root, result) {
                (true, Some(result)) if elements[index] == Element::Double => {
                    Output::Doubles(&mut values::reals_mut(result)[start..start + length])
                }
                (true, Some(result)) => {
                    Output::Integers(&mut values::ints_mut(result)[start..start + length])
                }
                _ if elements[index] == Element::Double => {
                    Output::Doubles(&mut real_rest[..length])
                }
                _ => Output::Integers(&mut integer_rest[..length]),
            };
            // A scalar operand beside a vector: see `payload_from_right`.
            let node = match *node {
                Node::Binary(operator @ (Arith::Add | Arith::Multiply), left, right)
                    if elements[index] == Element::Double
                        && scalar[left]
                        && !scalar[right]
                        && elements[left] != Element::Integer
                        && elements[right] != Element::Integer =>
                {
                    Node::Binary(operator, right, left)
                }
                node => node,
            };
            if !apply(node, view, output) {
                return None;
            }
        }
        if let Some(reduction) = reduction {
            let at = slots[root] * CHUNK;
            match (reduction, elements[root]) {
                (Reduction::Sum, Element::Double) => {
                    double_total =
                        long_double::add_all(double_total, &scratch.reals[at..at + length]);
                }
                (Reduction::Sum, _) => {
                    integer_total += sum_integers(&scratch.integers[at..at + length])?;
                }
                (Reduction::Any | Reduction::All, _) => {
                    let is_any = reduction == Reduction::Any;
                    let decided = any_all(is_any, &scratch.integers[at..at + length]);
                    if decided == i32::from(is_any) {
                        if !warns {
                            return Some(Total::Logical(decided));
                        }
                        settled = true;
                    }
                    missing |= decided == NA_LOGICAL;
                }
            }
        }
        start += length;
    }
    Some(match reduction {
        None => Total::Logical(0),
        Some(Reduction::Sum) if elements[root] == Element::Double => {
            // GNU R adds each argument's long double total to a double zero.
            Total::Double(0.0 + double_total.finite()?.to_f64())
        }
        Some(Reduction::Sum) => {
            let total = i32::try_from(integer_total)
                .ok()
                .filter(|&total| total != NA_INTEGER)?;
            Total::Integer(total)
        }
        Some(Reduction::Any) if settled => Total::Logical(1),
        Some(Reduction::All) if settled => Total::Logical(0),
        Some(Reduction::Any) => Total::Logical(if missing { NA_LOGICAL } else { 0 }),
        Some(Reduction::All) => Total::Logical(if missing { NA_LOGICAL } else { 1 }),
    })
}

/// One node of a fused tree over one chunk. `false` when the result would warn.
fn apply<'a>(node: Node, view: impl Fn(usize) -> View<'a>, output: Output<'_>) -> bool {
    let reals = |input: usize| match view(input) {
        View::Doubles(values) => Reals::Doubles(values),
        View::Integers(values) => Reals::Integers(values),
    };
    // Only integer and logical inputs reach an integer operator, as the type pass decided.
    let integers = |input: usize| match view(input) {
        View::Integers(values) => Integers::Values(values),
        View::Doubles(_) => Integers::Scalar(NA_INTEGER),
    };
    match (node, output) {
        (Node::Leaf(_), _) => true,
        (Node::Binary(operator, left, right), Output::Integers(output)) => {
            match comparison_of(operator) {
                Some(comparison)
                    if matches!(view(left), View::Integers(_))
                        && matches!(view(right), View::Integers(_)) =>
                {
                    compare_integers(comparison, integers(left), integers(right), output)
                }
                Some(comparison) => compare_reals(comparison, reals(left), reals(right), output),
                None => {
                    return !integer_arithmetic(
                        arithmetic_of(operator),
                        integers(left),
                        integers(right),
                        output,
                    );
                }
            }
            true
        }
        (Node::Binary(operator, left, right), Output::Doubles(output)) => {
            real_arithmetic(arithmetic_of(operator), reals(left), reals(right), output);
            true
        }
        (
            Node::Logic {
                is_and,
                left,
                right,
            },
            Output::Integers(output),
        ) => {
            logic(is_and, integers(left), integers(right), output);
            true
        }
        (Node::Negate(input), Output::Integers(output)) => {
            if let View::Integers(values) = view(input) {
                negate_integers(Some(values), output);
            }
            true
        }
        (Node::Negate(input), Output::Doubles(output)) => {
            if let View::Doubles(values) = view(input) {
                negate_reals(Some(values), output);
            }
            true
        }
        (Node::Not(input), Output::Integers(output)) => {
            if let View::Integers(values) = view(input) {
                not(Some(values), output);
            }
            true
        }
        (Node::Math(function, input), Output::Doubles(output)) => {
            !math(function, Some(reals(input)), output)
        }
        // The type pass gives every node the output its operator writes.
        _ => {
            debug_assert!(false, "a fused node got an output of the wrong type");
            false
        }
    }
}

// Macros that pick each operand's reader, defined before the kernels that use them.

/// Binds `$reader` to the reader for a numeric operand read as doubles, `None` being the
/// output buffer, and evaluates `$body` once per kind, so every combination of operand kinds
/// gets its own loop.
macro_rules! real_reader {
    ($operand:expr, |$reader:ident| $body:expr) => {
        match $operand.map(real_input) {
            Some(Reals::Doubles(values)) => {
                let $reader = Slice(values);
                $body
            }
            Some(Reals::Integers(values)) => {
                let $reader = Widened(values);
                $body
            }
            Some(Reals::Scalar(value)) => {
                let $reader = Constant(value);
                $body
            }
            None => {
                let $reader = Own;
                $body
            }
        }
    };
}

/// [`real_reader`] for an operand that cannot be the output, whose type differs.
macro_rules! input_reader {
    ($operand:expr, |$reader:ident| $body:expr) => {
        match real_input($operand) {
            Reals::Doubles(values) => {
                let $reader = Slice(values);
                $body
            }
            Reals::Integers(values) => {
                let $reader = Widened(values);
                $body
            }
            Reals::Scalar(value) => {
                let $reader = Constant(value);
                $body
            }
        }
    };
}

/// [`real_reader`] for an integer or logical operand.
macro_rules! integer_reader {
    ($operand:expr, |$reader:ident| $body:expr) => {
        match $operand.map(integer_input) {
            Some(Integers::Values(values)) => {
                let $reader = Slice(values);
                $body
            }
            Some(Integers::Scalar(value)) => {
                let $reader = Constant(value);
                $body
            }
            None => {
                let $reader = Own;
                $body
            }
        }
    };
}

// The kernels. `multiversion!` compiles each body for AVX2 and for the baseline.

macro_rules! multiversion {
    ($(#[$meta:meta])* pub fn $name:ident($($arg:ident: $ty:ty),* $(,)?) $(-> $ret:ty)? $body:block) => {
        $(#[$meta])*
        pub fn $name($($arg: $ty),*) $(-> $ret)? {
            #[inline(always)]
            fn body($($arg: $ty),*) $(-> $ret)? $body
            #[cfg(target_arch = "x86_64")]
            {
                #[target_feature(enable = "avx2")]
                fn wide($($arg: $ty),*) $(-> $ret)? {
                    body($($arg),*)
                }
                if std::arch::is_x86_feature_detected!("avx2") {
                    // SAFETY: the processor supports AVX2.
                    return unsafe { wide($($arg),*) };
                }
            }
            body($($arg),*)
        }
    };
}

multiversion! {
    /// `left op right` element by element into `output`, as doubles.
    pub fn real_arithmetic(op: Op, left: Reals<'_>, right: Reals<'_>, output: &mut [f64]) {
        real_operation(op, Some(left), Some(right), output);
    }
}

multiversion! {
    /// [`real_arithmetic`] where `output` holds the `side` operand on entry.
    pub fn real_arithmetic_into(op: Op, side: Side, other: Reals<'_>, output: &mut [f64]) {
        match side {
            Side::Left => real_operation(op, None, Some(other), output),
            Side::Right => real_operation(op, Some(other), None, output),
        }
    }
}

multiversion! {
    /// `left op right` on integers for `+`, `-`, `*`, `%%` and `%/%`, `NA` where either side is
    /// `NA`, a divisor is zero or a result leaves the integer range. Returns whether a result
    /// overflowed, which R warns about.
    pub fn integer_arithmetic(op: Op, left: Integers<'_>, right: Integers<'_>, output: &mut [i32]) -> bool {
        integer_operation(op, Some(left), Some(right), output)
    }
}

multiversion! {
    /// [`integer_arithmetic`] where `output` holds the `side` operand on entry.
    pub fn integer_arithmetic_into(op: Op, side: Side, other: Integers<'_>, output: &mut [i32]) -> bool {
        match side {
            Side::Left => integer_operation(op, None, Some(other), output),
            Side::Right => integer_operation(op, Some(other), None, output),
        }
    }
}

multiversion! {
    /// Compares two numeric operands as doubles, `NA` wherever either side is NaN.
    pub fn compare_reals(comparison: Comparison, left: Reals<'_>, right: Reals<'_>, output: &mut [i32]) {
        macro_rules! compare {
            ($holds:expr) => {
                input_reader!(left, |x| input_reader!(right, |y| map2(x, y, output, |a: f64, b: f64| {
                    if a.is_nan() | b.is_nan() { NA_LOGICAL } else { $holds(a, b) as i32 }
                })))
            };
        }
        match comparison {
            Comparison::Equal => compare!(|x, y| x == y),
            Comparison::NotEqual => compare!(|x, y| x != y),
            Comparison::Less => compare!(|x, y| x < y),
            Comparison::Greater => compare!(|x, y| x > y),
            Comparison::LessEqual => compare!(|x, y| x <= y),
            Comparison::GreaterEqual => compare!(|x, y| x >= y),
        }
    }
}

multiversion! {
    /// Compares two integer or logical operands, `NA` wherever either side is `NA`.
    pub fn compare_integers(comparison: Comparison, left: Integers<'_>, right: Integers<'_>, output: &mut [i32]) {
        macro_rules! compare {
            ($holds:expr) => {
                integer_reader!(Some(left), |x| integer_reader!(Some(right), |y| map2(x, y, output, |a: i32, b: i32| {
                    if (a == NA_INTEGER) | (b == NA_INTEGER) { NA_LOGICAL } else { $holds(a, b) as i32 }
                })))
            };
        }
        match comparison {
            Comparison::Equal => compare!(|x, y| x == y),
            Comparison::NotEqual => compare!(|x, y| x != y),
            Comparison::Less => compare!(|x, y| x < y),
            Comparison::Greater => compare!(|x, y| x > y),
            Comparison::LessEqual => compare!(|x, y| x <= y),
            Comparison::GreaterEqual => compare!(|x, y| x >= y),
        }
    }
}

multiversion! {
    /// `&` (`is_and`) or `|` on logicals: `FALSE & NA` is `FALSE` and `TRUE | NA` is `TRUE`.
    pub fn logic(is_and: bool, left: Integers<'_>, right: Integers<'_>, output: &mut [i32]) {
        logic_operation(is_and, Some(left), Some(right), output);
    }
}

multiversion! {
    /// [`logic`] where `output` holds the `side` operand on entry.
    pub fn logic_into(is_and: bool, side: Side, other: Integers<'_>, output: &mut [i32]) {
        match side {
            Side::Left => logic_operation(is_and, None, Some(other), output),
            Side::Right => logic_operation(is_and, Some(other), None, output),
        }
    }
}

multiversion! {
    /// `!` on logicals; with `source` `None`, `output` holds the operand on entry.
    pub fn not(source: Option<&[i32]>, output: &mut [i32]) {
        unary(source, output, |x| if x == NA_LOGICAL { NA_LOGICAL } else { 1 - x });
    }
}

multiversion! {
    /// Unary minus on integers; with `source` `None`, `output` holds the operand on entry.
    pub fn negate_integers(source: Option<&[i32]>, output: &mut [i32]) {
        unary(source, output, |x| if x == NA_INTEGER { NA_INTEGER } else { -x });
    }
}

multiversion! {
    /// Unary minus on doubles; with `source` `None`, `output` holds the operand on entry.
    pub fn negate_reals(source: Option<&[f64]>, output: &mut [f64]) {
        unary(source, output, |x: f64| -x);
    }
}

multiversion! {
    /// A function of the `Math` group on each element, `NA` staying `NA`. With `source` `None`,
    /// `output` holds the operand on entry. Returns whether a NaN came out of a value that was not
    /// NaN, which R warns about.
    pub fn math(function: Math, source: Option<Reals<'_>>, output: &mut [f64]) -> bool {
        // The common functions get their own loop, so the loop calls no function it cannot
        // vectorize.
        match function {
            Math::Floor => math_lanes(source, output, f64::floor),
            Math::Ceiling => math_lanes(source, output, f64::ceil),
            Math::Trunc => math_lanes(source, output, f64::trunc),
            Math::Abs => math_lanes(source, output, f64::abs),
            Math::Sqrt => math_lanes(source, output, f64::sqrt),
            Math::Exp => math_lanes(source, output, f64::exp),
            _ => math_lanes(source, output, |x| arith::math_value(function, x)),
        }
    }
}

multiversion! {
    /// The total of integers or logicals in 64 bits, or `None` when one is `NA`.
    pub fn sum_integers(values: &[i32]) -> Option<i64> {
        let mut total = 0i64;
        let mut missing = false;
        for &value in values {
            missing |= value == NA_INTEGER;
            total += i64::from(value);
        }
        (!missing).then_some(total)
    }
}

multiversion! {
    /// `any` (`is_any`) or `all` over logicals, as R's three-valued logic gives them.
    pub fn any_all(is_any: bool, values: &[i32]) -> i32 {
        // The decisive value ends the scan, a block at a time so the block's loop vectorizes.
        let decisive = if is_any { 1 } else { 0 };
        let mut missing = false;
        for block in values.chunks(1024) {
            let mut found = false;
            for &value in block {
                found |= value == decisive;
                missing |= value == NA_LOGICAL;
            }
            if found {
                return decisive;
            }
        }
        if missing { NA_LOGICAL } else { 1 - decisive }
    }
}

// Readers: how a loop reads one operand.

/// One operand as a loop reads it. `own` is the output's current element, which an operand
/// living in the output buffer reads before the loop overwrites it.
trait Reader<O>: Copy {
    type Item;
    /// The operand's length when it is a vector.
    fn length(self) -> Option<usize>;
    /// The operand from element `start` on.
    fn skip(self, start: usize) -> Self;
    /// The operand cut to `count` elements, so that the loop's reads need no bounds check.
    fn cut(self, count: usize) -> Self;
    fn read(self, index: usize, own: O) -> Self::Item;
}

#[derive(Clone, Copy)]
struct Slice<'a, T>(&'a [T]);

/// Integers or logicals read as doubles.
#[derive(Clone, Copy)]
struct Widened<'a>(&'a [i32]);

#[derive(Clone, Copy)]
struct Constant<T>(T);

/// The operand that the output buffer holds.
#[derive(Clone, Copy)]
struct Own;

impl<T: Copy, O> Reader<O> for Slice<'_, T> {
    type Item = T;

    #[inline(always)]
    fn length(self) -> Option<usize> {
        Some(self.0.len())
    }

    #[inline(always)]
    fn skip(self, start: usize) -> Self {
        Slice(&self.0[start..])
    }

    #[inline(always)]
    fn cut(self, count: usize) -> Self {
        Slice(&self.0[..count])
    }

    #[inline(always)]
    fn read(self, index: usize, _own: O) -> T {
        self.0[index]
    }
}

impl<O> Reader<O> for Widened<'_> {
    type Item = f64;

    #[inline(always)]
    fn length(self) -> Option<usize> {
        Some(self.0.len())
    }

    #[inline(always)]
    fn skip(self, start: usize) -> Self {
        Widened(&self.0[start..])
    }

    #[inline(always)]
    fn cut(self, count: usize) -> Self {
        Widened(&self.0[..count])
    }

    #[inline(always)]
    fn read(self, index: usize, _own: O) -> f64 {
        widen(self.0[index])
    }
}

impl<T: Copy, O> Reader<O> for Constant<T> {
    type Item = T;

    #[inline(always)]
    fn length(self) -> Option<usize> {
        None
    }

    #[inline(always)]
    fn skip(self, _start: usize) -> Self {
        self
    }

    #[inline(always)]
    fn cut(self, _count: usize) -> Self {
        self
    }

    #[inline(always)]
    fn read(self, _index: usize, _own: O) -> T {
        self.0
    }
}

impl<O> Reader<O> for Own {
    type Item = O;

    #[inline(always)]
    fn length(self) -> Option<usize> {
        None
    }

    #[inline(always)]
    fn skip(self, _start: usize) -> Self {
        self
    }

    #[inline(always)]
    fn cut(self, _count: usize) -> Self {
        self
    }

    #[inline(always)]
    fn read(self, _index: usize, own: O) -> O {
        own
    }
}

/// A [`Reals`] operand with a one-element vector read as a scalar, which is the common
/// recycling case and keeps the loop free of wrapping.
#[inline(always)]
fn real_input(operand: Reals<'_>) -> Reals<'_> {
    match operand {
        Reals::Doubles([value]) => Reals::Scalar(*value),
        Reals::Integers([value]) => Reals::Scalar(widen(*value)),
        other => other,
    }
}

#[inline(always)]
fn integer_input(operand: Integers<'_>) -> Integers<'_> {
    match operand {
        Integers::Values([value]) => Integers::Scalar(*value),
        other => other,
    }
}

/// Writes `function` of each pair of operand elements into `output`, recycling a shorter
/// vector operand: the output splits into runs in which neither operand wraps, so every run is
/// a straight loop over slices.
#[inline(always)]
fn map2<O: Copy, L: Reader<O>, R: Reader<O>>(
    left: L,
    right: R,
    output: &mut [O],
    mut function: impl FnMut(L::Item, R::Item) -> O,
) {
    let count = output.len();
    let fits = |length: Option<usize>| length.is_none_or(|length| length >= count);
    if fits(left.length()) && fits(right.length()) {
        zip(left, right, output, &mut function);
        return;
    }
    let mut start = 0;
    while start < count {
        let offset = |length: Option<usize>| length.map_or(0, |length| start % length);
        let run = |length: Option<usize>| length.map_or(count, |length| length - start % length);
        let end = count.min(start + run(left.length()).min(run(right.length())));
        zip(
            left.skip(offset(left.length())),
            right.skip(offset(right.length())),
            &mut output[start..end],
            &mut function,
        );
        start = end;
    }
}

#[inline(always)]
fn zip<O: Copy, L: Reader<O>, R: Reader<O>>(
    left: L,
    right: R,
    output: &mut [O],
    function: &mut impl FnMut(L::Item, R::Item) -> O,
) {
    let count = output.len();
    let (left, right) = (left.cut(count), right.cut(count));
    for index in 0..count {
        let own = output[index];
        output[index] = function(left.read(index, own), right.read(index, own));
    }
}

#[inline(always)]
fn real_operation(op: Op, left: Option<Reals<'_>>, right: Option<Reals<'_>>, output: &mut [f64]) {
    let square = matches!(right.map(real_input), Some(Reals::Scalar(exponent)) if exponent == 2.0);
    real_reader!(left, |x| real_reader!(right, |y| match op {
        Op::Add => map2(x, y, output, arith::add),
        Op::Subtract => map2(x, y, output, |a, b| a - b),
        Op::Multiply => map2(x, y, output, arith::multiply),
        Op::Divide => map2(x, y, output, |a, b| a / b),
        // `power` of a non-NaN base and 2 is the plain square, and gives a NaN base back
        // untouched, payload and all.
        Op::Power if square => map2(x, y, output, |a, _| if a.is_nan() { a } else { a * a }),
        Op::Power => map2(x, y, output, arith::power),
        Op::Modulo => map2(x, y, output, arith::real_modulo),
        Op::IntegerDivide => map2(x, y, output, arith::real_integer_divide),
    }))
}

#[inline(always)]
fn integer_operation(
    op: Op,
    left: Option<Integers<'_>>,
    right: Option<Integers<'_>>,
    output: &mut [i32],
) -> bool {
    let mut overflowed = false;
    integer_reader!(left, |x| integer_reader!(right, |y| match op {
        Op::Add => map2(x, y, output, |a: i32, b: i32| {
            let sum = a.wrapping_add(b);
            let missing = (a == NA_INTEGER) | (b == NA_INTEGER);
            let overflow = (((a ^ sum) & (b ^ sum)) < 0) | (sum == NA_INTEGER);
            overflowed |= overflow & !missing;
            if missing | overflow { NA_INTEGER } else { sum }
        }),
        Op::Subtract => map2(x, y, output, |a: i32, b: i32| {
            let difference = a.wrapping_sub(b);
            let missing = (a == NA_INTEGER) | (b == NA_INTEGER);
            let overflow = (((a ^ b) & (a ^ difference)) < 0) | (difference == NA_INTEGER);
            overflowed |= overflow & !missing;
            if missing | overflow {
                NA_INTEGER
            } else {
                difference
            }
        }),
        Op::Multiply => map2(x, y, output, |a: i32, b: i32| {
            let product = i64::from(a) * i64::from(b);
            let missing = (a == NA_INTEGER) | (b == NA_INTEGER);
            let overflow = (product > i64::from(i32::MAX)) | (product < -i64::from(i32::MAX));
            overflowed |= overflow & !missing;
            if missing | overflow {
                NA_INTEGER
            } else {
                product as i32
            }
        }),
        Op::Modulo => map2(x, y, output, |a: i32, b: i32| {
            if (a == NA_INTEGER) | (b == NA_INTEGER) | (b == 0) {
                NA_INTEGER
            } else {
                let remainder = a.rem_euclid(b);
                remainder + if b < 0 && remainder != 0 { b } else { 0 }
            }
        }),
        Op::IntegerDivide => map2(x, y, output, |a: i32, b: i32| {
            if (a == NA_INTEGER) | (b == NA_INTEGER) | (b == 0) {
                NA_INTEGER
            } else {
                (f64::from(a) / f64::from(b)).floor() as i32
            }
        }),
        // `/` and `^` on integers give doubles, through `real_arithmetic`.
        Op::Divide | Op::Power => {}
    }));
    overflowed
}

#[inline(always)]
fn logic_operation(
    is_and: bool,
    left: Option<Integers<'_>>,
    right: Option<Integers<'_>>,
    output: &mut [i32],
) {
    integer_reader!(left, |x| integer_reader!(right, |y| if is_and {
        map2(x, y, output, |a: i32, b: i32| {
            if (a == 0) | (b == 0) {
                0
            } else if (a == NA_LOGICAL) | (b == NA_LOGICAL) {
                NA_LOGICAL
            } else {
                1
            }
        })
    } else {
        map2(x, y, output, |a: i32, b: i32| {
            if (a == 1) | (b == 1) {
                1
            } else if (a == NA_LOGICAL) | (b == NA_LOGICAL) {
                NA_LOGICAL
            } else {
                0
            }
        })
    }))
}

#[inline(always)]
fn unary<T: Copy>(source: Option<&[T]>, output: &mut [T], function: impl Fn(T) -> T) {
    match source {
        Some(source) => {
            let source = &source[..output.len()];
            for (slot, &value) in output.iter_mut().zip(source) {
                *slot = function(value);
            }
        }
        None => {
            for slot in output.iter_mut() {
                *slot = function(*slot);
            }
        }
    }
}

#[inline(always)]
fn math_lanes(
    source: Option<Reals<'_>>,
    output: &mut [f64],
    function: impl Fn(f64) -> f64,
) -> bool {
    let mut produced_nan = false;
    real_reader!(source, |x| map2(
        x,
        Constant(()),
        output,
        |value: f64, _| {
            let result = if is_na_real(value) {
                NA_REAL
            } else {
                function(value)
            };
            produced_nan |= result.is_nan() & !value.is_nan();
            result
        }
    ));
    produced_nan
}

/// An integer or logical element as a double.
#[inline(always)]
pub fn widen(value: i32) -> f64 {
    if value == NA_INTEGER {
        NA_REAL
    } else {
        f64::from(value)
    }
}

