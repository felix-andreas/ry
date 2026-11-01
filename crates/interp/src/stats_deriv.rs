//! Symbolic differentiation: `D(expr, name)` and `deriv(expr, namevec, ...)`.
//!
//! A derivative is built by the chain rule from a table of known functions, and every node is
//! simplified as it is built (`0 * e` is `0`, `1 * e` is `e`, a unary minus is pulled out of a
//! product), so that results read the way R users know them. `deriv` then numbers the
//! distinct subexpressions of the function and its derivatives, keeps those used more than once
//! as `.exprN` variables and inlines the rest. Subexpressions are shared between the operands of
//! a derivative, and parentheses are added in place where deparsing needs them, which is why a
//! node can come out parenthesized in every place it appears.

use {
    crate::{
        builtin_libraries::NativeCall,
        unwind::{R, Unwind},
        values::{self, length, nil, symbols},
    },
    heap::object::{NA_REAL, Sexp, SexpType},
};

/// `doD(expr, name)`.
pub unsafe fn do_d(call: &NativeCall<'_>) -> R<Sexp> {
    let mut expression = call.arg(0);
    if expression.kind() == SexpType::Expression {
        expression = if length(expression) > 0 {
            values::list_elt(expression, 0)
        } else {
            nil()
        };
    }
    let names = call.arg(1);
    if names.kind() != SexpType::String || length(names) < 1 {
        return Err(call.error("variable must be a character string"));
    }
    if length(names) > 1 {
        call.warning("only the first element is used as variable name")?;
    }
    let name = values::string_at(names, 0)
        .map(|name| name.into_owned())
        .unwrap_or_default();
    let variable = values::install(&name)?;
    let derivative = differentiate(call, expression, variable)?;
    add_parens(derivative)
}

/// `deriv(expr, namevec, function.arg, tag, hessian)`.
pub unsafe fn deriv(call: &NativeCall<'_>) -> R<Sexp> {
    let mut expression = call.arg(0);
    if expression.kind() == SexpType::Expression {
        expression = if length(expression) > 0 {
            values::list_elt(expression, 0)
        } else {
            nil()
        };
    }
    let names_argument = call.arg(1);
    if names_argument.kind() != SexpType::String || length(names_argument) == 0 {
        return Err(call.error("invalid variable names"));
    }
    let names: Vec<String> = values::strings(names_argument)
        .into_iter()
        .map(Option::unwrap_or_default)
        .collect();
    let function_argument = call.arg(2);
    let tag = match values::first_string(call.arg(3)) {
        Some(tag) if !tag.is_empty() && tag.len() <= 60 => tag.into_owned(),
        _ => return Err(call.error("invalid tag")),
    };
    let hessian = values::as_logical_scalar(call.arg(4)) == 1;
    let variables: Vec<Sexp> = names
        .iter()
        .map(|name| values::install(name))
        .collect::<R<_>>()?;
    let count = names.len();

    let mut table = Subexpressions {
        entries: Vec::new(),
        tag: tag.clone(),
    };
    let f_index = table.find(call, values::duplicate(expression)?)?;
    let mut d_index = vec![0usize; count];
    let mut d2_index = Vec::new();
    for i in 0..count {
        let first = differentiate(call, values::duplicate(expression)?, variables[i])?;
        let index = table.find(call, values::duplicate(first)?)?;
        d_index[i] = index;
        if hessian {
            for &variable in &variables[i..] {
                let second = differentiate(call, values::duplicate(first)?, variable)?;
                let index = table.find(call, second)?;
                d2_index.push(index);
            }
        }
    }
    let subexpression_count = table.entries.len();
    let mut entries = table.entries;
    let variable_for = |index: usize| values::install(&format!("{tag}{index}"));
    entries.push(if f_index != 0 {
        variable_for(f_index)?
    } else {
        values::duplicate(expression)?
    });
    let slot = values::missing_arg();
    entries.push(nil());
    if hessian {
        entries.push(nil());
    }
    let mut k = 0;
    for i in 0..count {
        let first = if d_index[i] != 0 {
            entries.push(variable_for(d_index[i])?);
            None
        } else {
            let derivative = differentiate(call, values::duplicate(expression)?, variables[i])?;
            entries.push(derivative);
            Some(derivative)
        };
        if hessian {
            let first = match first {
                Some(first) => first,
                None => differentiate(call, values::duplicate(expression)?, variables[i])?,
            };
            for j in i..count {
                if d2_index[k] != 0 {
                    entries.push(variable_for(d2_index[k])?);
                } else {
                    let second = differentiate(call, values::duplicate(first)?, variables[j])?;
                    entries.push(if d_index[i] == 0 && is_zero(second) {
                        slot
                    } else {
                        second
                    });
                }
                k += 1;
            }
        }
    }
    entries.push(nil());
    entries.push(nil());
    if hessian {
        entries.push(nil());
    }

    // Inline the subexpressions used only once; keep the others as `.exprN <- ...`.
    for i in 0..subexpression_count {
        let variable = variable_for(i + 1)?;
        let later_uses: usize = entries[i + 1..]
            .iter()
            .map(|&entry| count_occurrences(variable, entry))
            .sum();
        if later_uses < 2 {
            let replacement = entries[i];
            for entry in entries[i + 1..].iter_mut() {
                *entry = replace(variable, replacement, *entry);
            }
            entries[i] = slot;
        } else {
            let value = add_parens(entries[i])?;
            entries[i] = values::call(symbols().assign, &[variable, value])?;
        }
    }
    let assign = symbols().assign;
    let mut position = subexpression_count;
    let value_symbol = values::install(".value")?;
    let value = add_parens(entries[position])?;
    entries[position] = values::call(assign, &[value_symbol, value])?;
    position += 1;
    let names_vector = names_call(&names)?;
    let length_call = values::call(values::install("length")?, &[value_symbol])?;
    let size = values::scalar_integer(count as i32)?;
    let array = values::install("array")?;
    let zero = values::scalar_real(0.0)?;
    let combine = values::install("c")?;
    let list = values::install("list")?;
    let gradient_symbol = values::install(".grad")?;
    let dims = values::call(combine, &[length_call, size])?;
    let dimnames = values::call(list, &[nil(), names_vector])?;
    let create = values::call(array, &[zero, dims, dimnames])?;
    entries[position] = values::call(assign, &[gradient_symbol, create])?;
    position += 1;
    let hessian_symbol = values::install(".hessian")?;
    if hessian {
        let dims = values::call(combine, &[length_call, size, size])?;
        let dimnames = values::call(list, &[nil(), names_vector, names_vector])?;
        let create = values::call(array, &[zero, dims, dimnames])?;
        entries[position] = values::call(assign, &[hessian_symbol, create])?;
        position += 1;
    }
    let subscript = |target: Sexp, indices: &[&str]| -> R<Sexp> {
        let mut items = vec![
            (nil(), symbols().bracket),
            (nil(), target),
            (nil(), values::missing_arg()),
        ];
        for index in indices {
            items.push((nil(), values::scalar_string(index)?));
        }
        values::pairlist(&items, SexpType::Language)
    };
    for i in 0..count {
        let value = add_parens(entries[position])?;
        let target = subscript(gradient_symbol, &[&names[i]])?;
        entries[position] = values::call(assign, &[target, value])?;
        position += 1;
        if hessian {
            for j in i..count {
                if entries[position] != slot {
                    let value = add_parens(entries[position])?;
                    entries[position] = if i == j {
                        let target = subscript(hessian_symbol, &[&names[i], &names[i]])?;
                        values::call(assign, &[target, value])?
                    } else {
                        let first_target = subscript(hessian_symbol, &[&names[i], &names[j]])?;
                        let second_target = subscript(hessian_symbol, &[&names[j], &names[i]])?;
                        let inner = values::call(assign, &[second_target, value])?;
                        values::call(assign, &[first_target, inner])?
                    };
                }
                position += 1;
            }
        }
    }
    let attribute = values::install("attr")?;
    let gradient_attribute = values::call(
        attribute,
        &[value_symbol, values::scalar_string("gradient")?],
    )?;
    entries[position] = values::call(assign, &[gradient_attribute, gradient_symbol])?;
    position += 1;
    if hessian {
        let hessian_attribute = values::call(
            attribute,
            &[value_symbol, values::scalar_string("hessian")?],
        )?;
        entries[position] = values::call(assign, &[hessian_attribute, hessian_symbol])?;
        position += 1;
    }
    entries[position] = value_symbol;
    let kept: Vec<Sexp> = entries.into_iter().filter(|&entry| entry != slot).collect();
    let body = values::call(symbols().brace, &kept)?;

    let function_argument = if function_argument.kind() == SexpType::Logical
        && values::as_logical_scalar(function_argument) == 1
    {
        names_argument
    } else {
        function_argument
    };
    match function_argument.kind() {
        SexpType::Closure => crate::eval::make_closure(
            function_argument.car(),
            body,
            values::nil(),
            function_argument.tag(),
        ),
        SexpType::String => {
            let mut formals = Vec::with_capacity(length(function_argument));
            for name in values::strings(function_argument).into_iter().flatten() {
                formals.push((values::install(&name)?, values::missing_arg()));
            }
            let formals = values::pairlist(&formals, SexpType::Pairlist)?;
            crate::eval::make_closure(formals, body, values::nil(), values::global_env())
        }
        _ => {
            let result = values::alloc(SexpType::Expression, 1)?;
            values::set_elt(result, 0, body);
            Ok(result)
        }
    }
}

/// The subexpressions of `deriv`, numbered from 1 in order of first appearance.
struct Subexpressions {
    entries: Vec<Sexp>,
    tag: String,
}

impl Subexpressions {
    /// Replaces, in place, each call among the arguments of `expression` by its numbered
    /// variable, and returns the number of the expression itself (0 for a symbol or constant).
    unsafe fn find(&mut self, call: &NativeCall<'_>, expression: Sexp) -> R<usize> {
        match expression.kind() {
            SexpType::Symbol
            | SexpType::Logical
            | SexpType::Integer
            | SexpType::Real
            | SexpType::Complex => Ok(0),
            SexpType::Language => {
                if expression.car() == symbols().function {
                    return Ok(0);
                }
                // Parentheses are not subexpressions of their own.
                if expression.car() == symbols().paren && length(expression) == 2 {
                    return self.find(call, expression.cdr().car());
                }
                let mut cell = expression.cdr();
                while cell != nil() {
                    let index = self.find(call, cell.car())?;
                    if index != 0 {
                        crate::heap()
                            .set_car(cell, values::install(&format!("{}{index}", self.tag))?);
                    }
                    cell = cell.cdr();
                }
                if let Some(position) = self
                    .entries
                    .iter()
                    .position(|&entry| equal(entry, expression))
                {
                    return Ok(position + 1);
                }
                self.entries.push(expression);
                Ok(self.entries.len())
            }
            _ => Err(call.error("invalid expression in 'FindSubexprs'")),
        }
    }
}

/// Structural equality of expressions, comparing constants by their first value.
unsafe fn equal(left: Sexp, right: Sexp) -> bool {
    if left.kind() != right.kind() {
        return false;
    }
    match left.kind() {
        SexpType::Nil => true,
        SexpType::Symbol => left == right,
        SexpType::Logical | SexpType::Integer => {
            values::ints(left).first() == values::ints(right).first()
        }
        SexpType::Real => values::reals(left).first() == values::reals(right).first(),
        SexpType::Complex => {
            let (a, b) = (
                values::complexes(left).first(),
                values::complexes(right).first(),
            );
            matches!((a, b), (Some(a), Some(b)) if a.r == b.r && a.i == b.i)
        }
        SexpType::Language | SexpType::Pairlist => {
            equal(left.car(), right.car()) && equal(left.cdr(), right.cdr())
        }
        _ => false,
    }
}

unsafe fn count_occurrences(symbol: Sexp, expression: Sexp) -> usize {
    match expression.kind() {
        SexpType::Symbol => (expression == symbol) as usize,
        SexpType::Language | SexpType::Pairlist => {
            count_occurrences(symbol, expression.car())
                + count_occurrences(symbol, expression.cdr())
        }
        _ => 0,
    }
}

/// `expression` with `symbol` replaced by `replacement`, in place.
unsafe fn replace(symbol: Sexp, replacement: Sexp, expression: Sexp) -> Sexp {
    match expression.kind() {
        SexpType::Symbol if expression == symbol => replacement,
        SexpType::Language | SexpType::Pairlist => {
            let car = replace(symbol, replacement, expression.car());
            crate::heap().set_car(expression, car);
            let cdr = replace(symbol, replacement, expression.cdr());
            crate::heap().set_cdr(expression, cdr);
            expression
        }
        _ => expression,
    }
}

unsafe fn names_call(names: &[String]) -> R<Sexp> {
    let items: Vec<Sexp> = names
        .iter()
        .map(|name| values::scalar_string(name))
        .collect::<R<_>>()?;
    values::call(values::install("c")?, &items)
}

// The derivative and its simplification.

unsafe fn constant(value: f64) -> R<Sexp> {
    values::scalar_real(value)
}

unsafe fn is_numeric_constant(expression: Sexp) -> bool {
    matches!(
        expression.kind(),
        SexpType::Logical | SexpType::Integer | SexpType::Real
    ) && length(expression) >= 1
}

unsafe fn constant_value(expression: Sexp) -> Option<f64> {
    is_numeric_constant(expression).then(|| values::as_real_scalar(expression))
}

unsafe fn is_zero(expression: Sexp) -> bool {
    constant_value(expression) == Some(0.0)
}

unsafe fn is_one(expression: Sexp) -> bool {
    constant_value(expression) == Some(1.0)
}

unsafe fn is_call(expression: Sexp, name: &str) -> bool {
    expression.kind() == SexpType::Language
        && expression.car().kind() == SexpType::Symbol
        && values::symbol_name(expression.car()) == name
}

unsafe fn is_unary_minus(expression: Sexp) -> bool {
    is_call(expression, "-")
        && (length(expression) == 2
            || (length(expression) == 3 && expression.cdr().cdr().car() == values::missing_arg()))
}

unsafe fn second(expression: Sexp) -> Sexp {
    expression.cdr().car()
}

unsafe fn third(expression: Sexp) -> Sexp {
    expression.cdr().cdr().car()
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Operator {
    Plus,
    Minus,
    Times,
    Divide,
    Power,
}

/// Builds `left op right` (`right` is `None` for a unary operator), simplified.
unsafe fn simplify(operator: Operator, left: Sexp, right: Option<Sexp>) -> R<Sexp> {
    let symbol = |name: &str| values::install(name);
    match operator {
        Operator::Plus => {
            let Some(right) = right else { return Ok(left) };
            if is_zero(left) {
                Ok(right)
            } else if is_zero(right) {
                Ok(left)
            } else if is_unary_minus(left) {
                simplify(Operator::Minus, right, Some(second(left)))
            } else if is_unary_minus(right) {
                simplify(Operator::Minus, left, Some(second(right)))
            } else {
                values::call(symbol("+")?, &[left, right])
            }
        }
        Operator::Minus => match right {
            None => {
                if is_zero(left) {
                    constant(0.0)
                } else if is_unary_minus(left) {
                    Ok(second(left))
                } else {
                    values::call(symbol("-")?, &[left])
                }
            }
            Some(right) => {
                if is_zero(right) {
                    Ok(left)
                } else if is_zero(left) {
                    simplify(Operator::Minus, right, None)
                } else if is_unary_minus(left) {
                    let sum = simplify(Operator::Plus, second(left), Some(right))?;
                    simplify(Operator::Minus, sum, None)
                } else if is_unary_minus(right) {
                    values::call(symbol("+")?, &[left, second(right)])
                } else {
                    values::call(symbol("-")?, &[left, right])
                }
            }
        },
        Operator::Times => {
            let right = right.unwrap_or_else(nil);
            if is_zero(left) || is_zero(right) {
                constant(0.0)
            } else if is_one(left) {
                Ok(right)
            } else if is_one(right) {
                Ok(left)
            } else if is_unary_minus(left) {
                let product = simplify(Operator::Times, second(left), Some(right))?;
                simplify(Operator::Minus, product, None)
            } else if is_unary_minus(right) {
                let product = simplify(Operator::Times, left, Some(second(right)))?;
                simplify(Operator::Minus, product, None)
            } else {
                values::call(symbol("*")?, &[left, right])
            }
        }
        Operator::Divide => {
            let right = right.unwrap_or_else(nil);
            if is_zero(left) {
                constant(0.0)
            } else if is_zero(right) {
                constant(NA_REAL)
            } else if is_one(right) {
                Ok(left)
            } else if is_unary_minus(left) {
                let quotient = simplify(Operator::Divide, second(left), Some(right))?;
                simplify(Operator::Minus, quotient, None)
            } else if is_unary_minus(right) {
                let quotient = simplify(Operator::Divide, left, Some(second(right)))?;
                simplify(Operator::Minus, quotient, None)
            } else {
                values::call(symbol("/")?, &[left, right])
            }
        }
        Operator::Power => {
            let right = right.unwrap_or_else(nil);
            if is_one(right) {
                Ok(left)
            } else if is_zero(right) {
                constant(1.0)
            } else if is_zero(left) {
                constant(0.0)
            } else if is_unary_minus(left)
                && right.kind() == SexpType::Integer
                && values::as_integer_scalar(right) % 2 == 0
            {
                simplify(Operator::Power, second(left), Some(right))
            } else {
                values::call(symbol("^")?, &[left, right])
            }
        }
    }
}

/// `name(argument)`, or `name(argument, extra)`.
unsafe fn function_call(name: &str, argument: Sexp, extra: Option<Sexp>) -> R<Sexp> {
    let symbol = values::install(name)?;
    match extra {
        Some(extra) => values::call(symbol, &[argument, extra]),
        None => values::call(symbol, &[argument]),
    }
}

/// The derivative of `expression` with respect to the symbol `variable`.
unsafe fn differentiate(call: &NativeCall<'_>, expression: Sexp, variable: Sexp) -> R<Sexp> {
    use Operator::{Divide, Minus, Plus, Power, Times};
    match expression.kind() {
        SexpType::Logical | SexpType::Integer | SexpType::Real | SexpType::Complex => {
            return constant(0.0);
        }
        SexpType::Symbol => return constant(if expression == variable { 1.0 } else { 0.0 }),
        SexpType::Language => {}
        _ => return Err(call.error("invalid expression in 'D'")),
    }
    let head = expression.car();
    if head.kind() != SexpType::Symbol {
        return Err(not_in_table(call, &crate::deparse::deparse_one_line(head)));
    }
    let name = values::symbol_name(head).into_owned();
    let arguments = length(expression) - 1;
    let a = if arguments >= 1 {
        second(expression)
    } else {
        nil()
    };
    let b = if arguments >= 2 {
        third(expression)
    } else {
        nil()
    };
    let derivative = |argument: Sexp| differentiate(call, argument, variable);
    match name.as_str() {
        "(" => derivative(a),
        "+" if arguments == 1 => simplify(Plus, derivative(a)?, None),
        "+" => {
            let (left, right) = (derivative(a)?, derivative(b)?);
            simplify(Plus, left, Some(right))
        }
        "-" if arguments == 1 => simplify(Minus, derivative(a)?, None),
        "-" => {
            let (left, right) = (derivative(a)?, derivative(b)?);
            simplify(Minus, left, Some(right))
        }
        "*" => {
            let left = simplify(Times, derivative(a)?, Some(b))?;
            let right = simplify(Times, a, Some(derivative(b)?))?;
            simplify(Plus, left, Some(right))
        }
        "/" => {
            let left = simplify(Divide, derivative(a)?, Some(b))?;
            let numerator = simplify(Times, a, Some(derivative(b)?))?;
            let square = simplify(Power, b, Some(constant(2.0)?))?;
            let right = simplify(Divide, numerator, Some(square))?;
            simplify(Minus, left, Some(right))
        }
        "^" => {
            if is_numeric_constant(b) {
                let lowered = simplify(Power, a, Some(constant(values::as_real_scalar(b) - 1.0)?))?;
                let inner = simplify(Times, derivative(a)?, Some(lowered))?;
                simplify(Times, b, Some(inner))
            } else {
                let exponent = simplify(Minus, b, Some(constant(1.0)?))?;
                let lowered = simplify(Power, a, Some(exponent))?;
                let factor = simplify(Times, b, Some(derivative(a)?))?;
                let first = simplify(Times, lowered, Some(factor))?;
                let power = simplify(Power, a, Some(b))?;
                let logarithm = function_call("log", a, None)?;
                let factor = simplify(Times, logarithm, Some(derivative(b)?))?;
                let second = simplify(Times, power, Some(factor))?;
                simplify(Plus, first, Some(second))
            }
        }
        "exp" => simplify(Times, expression, Some(derivative(a)?)),
        "log" => {
            if arguments != 1 {
                return Err(call.error("only single-argument calls to log() are supported;\n  maybe use log(x,a) = log(x)/log(a)"));
            }
            simplify(Divide, derivative(a)?, Some(a))
        }
        "cos" => {
            let sine = function_call("sin", a, None)?;
            let negated = simplify(Minus, derivative(a)?, None)?;
            simplify(Times, sine, Some(negated))
        }
        "sin" => simplify(Times, function_call("cos", a, None)?, Some(derivative(a)?)),
        "tan" => {
            let square = simplify(Power, function_call("cos", a, None)?, Some(constant(2.0)?))?;
            simplify(Divide, derivative(a)?, Some(square))
        }
        "cosh" => simplify(Times, function_call("sinh", a, None)?, Some(derivative(a)?)),
        "sinh" => simplify(Times, function_call("cosh", a, None)?, Some(derivative(a)?)),
        "tanh" => {
            let square = simplify(Power, function_call("cosh", a, None)?, Some(constant(2.0)?))?;
            simplify(Divide, derivative(a)?, Some(square))
        }
        "sqrt" => {
            let power = values::call(values::install("^")?, &[a, constant(0.5)?])?;
            derivative(power)
        }
        "pnorm" => simplify(
            Times,
            function_call("dnorm", a, None)?,
            Some(derivative(a)?),
        ),
        "dnorm" => {
            let inner = simplify(
                Times,
                function_call("dnorm", a, None)?,
                Some(derivative(a)?),
            )?;
            let product = simplify(Times, a, Some(inner))?;
            simplify(Minus, product, None)
        }
        "asin" | "acos" => {
            let square = simplify(Power, a, Some(constant(2.0)?))?;
            let difference = simplify(Minus, constant(1.0)?, Some(square))?;
            let root = function_call("sqrt", difference, None)?;
            let quotient = simplify(Divide, derivative(a)?, Some(root))?;
            if name == "asin" {
                Ok(quotient)
            } else {
                simplify(Minus, quotient, None)
            }
        }
        "atan" => {
            let square = simplify(Power, a, Some(constant(2.0)?))?;
            let sum = simplify(Plus, constant(1.0)?, Some(square))?;
            simplify(Divide, derivative(a)?, Some(sum))
        }
        "lgamma" => simplify(
            Times,
            derivative(a)?,
            Some(function_call("digamma", a, None)?),
        ),
        "gamma" => {
            let product = simplify(Times, expression, Some(function_call("digamma", a, None)?))?;
            simplify(Times, derivative(a)?, Some(product))
        }
        "digamma" => simplify(
            Times,
            derivative(a)?,
            Some(function_call("trigamma", a, None)?),
        ),
        "trigamma" => {
            let polygamma = function_call("psigamma", a, Some(values::scalar_integer(2)?))?;
            simplify(Times, derivative(a)?, Some(polygamma))
        }
        "psigamma" => {
            let order = if arguments == 1 {
                values::scalar_integer(1)?
            } else if matches!(b.kind(), SexpType::Integer | SexpType::Real) {
                values::scalar_integer(values::as_integer_scalar(b) + 1)?
            } else {
                simplify(Plus, b, Some(values::scalar_integer(1)?))?
            };
            let polygamma = function_call("psigamma", a, Some(order))?;
            simplify(Times, derivative(a)?, Some(polygamma))
        }
        "log1p" => {
            let sum = simplify(Plus, constant(1.0)?, Some(a))?;
            simplify(Divide, derivative(a)?, Some(sum))
        }
        "expm1" => simplify(Times, function_call("exp", a, None)?, Some(derivative(a)?)),
        "log2" | "log10" => {
            let base = function_call(
                "log",
                constant(if name == "log2" { 2.0 } else { 10.0 })?,
                None,
            )?;
            let product = simplify(Times, a, Some(base))?;
            simplify(Divide, derivative(a)?, Some(product))
        }
        "cospi" | "sinpi" => {
            let other = function_call(if name == "cospi" { "sinpi" } else { "cospi" }, a, None)?;
            let scaled = simplify(Times, values::install("pi")?, Some(derivative(a)?))?;
            let product = simplify(Times, other, Some(scaled))?;
            if name == "cospi" {
                simplify(Minus, product, None)
            } else {
                Ok(product)
            }
        }
        "tanpi" => {
            let scaled = simplify(Times, values::install("pi")?, Some(derivative(a)?))?;
            let square = simplify(
                Power,
                function_call("cospi", a, None)?,
                Some(constant(2.0)?),
            )?;
            simplify(Divide, scaled, Some(square))
        }
        "lfactorial" | "factorial" => {
            let shifted = simplify(Plus, a, Some(values::scalar_integer(1)?))?;
            let polygamma = function_call("digamma", shifted, None)?;
            let factor = if name == "factorial" {
                simplify(Times, expression, Some(polygamma))?
            } else {
                polygamma
            };
            simplify(Times, derivative(a)?, Some(factor))
        }
        _ => Err(not_in_table(call, &name)),
    }
}

fn not_in_table(call: &NativeCall<'_>, name: &str) -> Unwind {
    call.error(format!("Function '{name}' is not in the derivatives table"))
}

/// Parenthesizes, in place, the operands that deparsing would otherwise misread: a sum or
/// difference on the right of `+` or `-`, any arithmetic on the right of `*` or `/` and a
/// sum or difference on their left, and arithmetic in an exponent or a power as a base.
unsafe fn add_parens(expression: Sexp) -> R<Sexp> {
    if expression.kind() == SexpType::Language {
        let mut cell = expression.cdr();
        while cell != nil() {
            let child = add_parens(cell.car())?;
            crate::heap().set_car(cell, child);
            cell = cell.cdr();
        }
    }
    let binary = |name: &str, value: Sexp| is_call(value, name) && length(value) == 3;
    let sum_like = |value: Sexp| binary("+", value) || binary("-", value);
    let arithmetic = |value: Sexp| sum_like(value) || binary("*", value) || binary("/", value);
    let wrap_right = |expression: Sexp| -> R<()> {
        let cell = expression.cdr().cdr();
        let wrapped = values::call(symbols().paren, &[cell.car()])?;
        crate::heap().set_car(cell, wrapped);
        Ok(())
    };
    let wrap_left = |expression: Sexp| -> R<()> {
        let cell = expression.cdr();
        let wrapped = values::call(symbols().paren, &[cell.car()])?;
        crate::heap().set_car(cell, wrapped);
        Ok(())
    };
    if binary("+", expression) {
        if binary("+", third(expression)) {
            wrap_right(expression)?;
        }
    } else if binary("-", expression) {
        if sum_like(third(expression)) {
            wrap_right(expression)?;
        }
    } else if binary("*", expression) || binary("/", expression) {
        if arithmetic(third(expression)) {
            wrap_right(expression)?;
        }
        if sum_like(second(expression)) {
            wrap_left(expression)?;
        }
    } else if binary("^", expression) {
        if binary("^", second(expression)) {
            wrap_left(expression)?;
        }
        if arithmetic(third(expression)) {
            wrap_right(expression)?;
        }
    }
    Ok(expression)
}
