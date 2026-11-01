//! Model formulas: `terms` (the expansion of a formula's operators into terms), `update` of a
//! formula, model frames and design matrices, and the leave-one-out diagnostics of a QR fit.
//!
//! A term is a set of variables, kept as their sorted indices into the formula's variable list.
//! A formula expands under the operator algebra Wilkinson and Rogers describe and the S model
//! language (Chambers and Hastie, "Statistical Models in S") made standard: `+` unites term
//! lists, `:` forms interactions, `*` crosses, `/` and `%in%` nest, `^` crosses a list with
//! itself and `-` removes terms.

use {
    crate::{
        builtin_libraries::NativeCall,
        deparse::{deparse_lines, options},
        eval, linalg,
        sort::{IdenticalFlags, identical},
        stats::{as_reals, matrix_shape},
        unwind::R,
        values::{self, length, nil, symbols},
    },
    heap::object::{NA_INTEGER, NA_REAL, Sexp, SexpType},
};

/// `termsform(x, specials, data, keep.order, allowDotAsName)`: the `terms` object of a formula.
pub unsafe fn termsform(call: &NativeCall<'_>) -> R<Sexp> {
    let formula = call.arg(0);
    let specials = call.arg(1);
    let data = call.arg(2);
    let keep_order = values::as_logical_scalar(call.arg(3)) == 1;
    let allow_dot_as_name = values::as_logical_scalar(call.arg(4)) == 1;
    if !is_call_to(formula, "~") || !matches!(length(formula), 2 | 3) {
        return Err(call.error("argument is not a valid model"));
    }
    if specials != nil() && specials.kind() != SexpType::String {
        return Err(call.error("'specials' must be NULL or a character vector"));
    }
    let frame_names: Vec<String> = match data.kind() {
        SexpType::List => values::strings(values::names(data))
            .into_iter()
            .flatten()
            .collect(),
        _ => Vec::new(),
    };
    let has_response = length(formula) == 3;
    let mut model = Model {
        variables: Vec::new(),
        intercept: true,
        parity: true,
        frame_names,
        allow_dot_as_name,
        call: call.call,
    };
    if has_response {
        let lhs = formula.cdr().car();
        model.install(lhs)?;
        // Data columns named on the left-hand side are not part of `.` on the right.
        let mut on_lhs = Vec::new();
        lhs_symbols(lhs, &mut on_lhs);
        model.frame_names.retain(|name| !on_lhs.contains(name));
    }
    let rhs = if has_response {
        formula.cdr().cdr().car()
    } else {
        formula.cdr().car()
    };
    model.extract(rhs)?;
    let mut terms = model.encode(rhs)?;
    let offsets: Vec<usize> = (0..model.variables.len())
        .filter(|&index| is_call_to(model.variables[index], "offset"))
        .collect();
    terms.retain(|term| !term.iter().any(|variable| offsets.contains(variable)));
    if !keep_order {
        terms.sort_by_key(Vec::len);
    }

    let result = values::shallow_duplicate(formula)?;
    let environment = values::get_attr(formula, symbols().dot_environment);
    values::clear_attributes(result);
    if !model.frame_names.is_empty() {
        let expanded = expand_frame_dot(rhs, &model.frame_names)?;
        let tail = if has_response {
            result.cdr().cdr()
        } else {
            result.cdr()
        };
        crate::heap().set_car(tail, expanded);
    }

    let variable_count = model.variables.len();
    let variable_names: Vec<String> = model
        .variables
        .iter()
        .map(|&variable| variable_label(variable))
        .collect();
    let variables = values::call(values::install("list")?, &model.variables)?;
    values::set_attr(result, values::install("variables")?, variables)?;
    if !offsets.is_empty() {
        let indices: Vec<i32> = offsets.iter().map(|&index| index as i32 + 1).collect();
        values::set_attr(
            result,
            values::install("offset")?,
            values::integer_vector(&indices)?,
        )?;
    }
    let term_labels: Vec<String> = terms
        .iter()
        .map(|term| {
            term.iter()
                .map(|&variable| variable_names[variable].as_str())
                .collect::<Vec<_>>()
                .join(":")
        })
        .collect();
    let factors = if terms.is_empty() {
        values::alloc(SexpType::Integer, 0)?
    } else {
        let matrix = values::alloc(SexpType::Integer, variable_count * terms.len())?;
        let codes = values::ints_mut(matrix);
        codes.fill(0);
        for (position, term) in terms.iter().enumerate() {
            for &variable in term {
                codes[variable + position * variable_count] = term_code(&terms, position, variable);
            }
        }
        let dim = values::integer_vector(&[variable_count as i32, terms.len() as i32])?;
        values::set_attr(matrix, symbols().dim, dim)?;
        let dimnames = values::list_vector(&[
            values::string_vector(&variable_names)?,
            values::string_vector(&term_labels)?,
        ])?;
        values::set_attr(matrix, symbols().dimnames, dimnames)?;
        matrix
    };
    values::set_attr(result, values::install("factors")?, factors)?;
    values::set_attr(
        result,
        values::install("term.labels")?,
        values::string_vector(&term_labels)?,
    )?;
    if specials != nil() {
        let mut items = Vec::new();
        for name in values::strings(specials).into_iter().flatten() {
            let indices: Vec<i32> = (0..variable_count)
                .filter(|&index| is_call_to(model.variables[index], &name))
                .map(|index| index as i32 + 1)
                .collect();
            let value = if indices.is_empty() {
                nil()
            } else {
                values::integer_vector(&indices)?
            };
            items.push((values::install(&name)?, value));
        }
        let table = values::pairlist(&items, SexpType::Pairlist)?;
        values::set_attr(result, values::install("specials")?, table)?;
    }
    let orders: Vec<i32> = terms.iter().map(|term| term.len() as i32).collect();
    values::set_attr(
        result,
        values::install("order")?,
        values::integer_vector(&orders)?,
    )?;
    values::set_attr(
        result,
        values::install("intercept")?,
        values::scalar_integer(model.intercept as i32)?,
    )?;
    values::set_attr(
        result,
        values::install("response")?,
        values::scalar_integer(has_response as i32)?,
    )?;
    values::set_attr(
        result,
        symbols().class,
        values::string_vector(&["terms", "formula"])?,
    )?;
    values::set_attr(result, symbols().dot_environment, environment)?;
    Ok(result)
}

/// `updateform(old, new)`: `new` with each `.` replaced by the matching side of `old`.
pub unsafe fn updateform(call: &NativeCall<'_>) -> R<Sexp> {
    let old = call.arg(0);
    let new = values::duplicate(call.arg(1))?;
    if !is_call_to(old, "~") || !is_call_to(new, "~") {
        return Err(call.error("formula expected"));
    }
    let heap = crate::heap();
    if length(old) == 3 {
        let lhs = old.cdr().car();
        let rhs = old.cdr().cdr().car();
        if length(new) == 2 {
            let cell = values::cons(lhs, new.cdr())?;
            heap.set_cdr(new, cell);
        }
        let expanded = expand_dots(new.cdr().car(), lhs)?;
        heap.set_car(new.cdr(), expanded);
        let expanded = expand_dots(new.cdr().cdr().car(), rhs)?;
        heap.set_car(new.cdr().cdr(), expanded);
    } else {
        let rhs = old.cdr().car();
        let cell = if length(new) == 3 {
            new.cdr().cdr()
        } else {
            new.cdr()
        };
        let expanded = expand_dots(cell.car(), rhs)?;
        heap.set_car(cell, expanded);
    }
    let environment_symbol = symbols().dot_environment;
    let environment = values::get_attr(new, environment_symbol);
    values::clear_attributes(new);
    values::set_attr(new, environment_symbol, environment)?;
    Ok(new)
}

/// `modelframe(terms, rownames, variables, varnames, dots, dotnames, subset, na.action)`.
pub unsafe fn modelframe(call: &NativeCall<'_>) -> R<Sexp> {
    let terms = call.arg(0);
    let row_names = call.arg(1);
    let variables = call.arg(2);
    let variable_names = call.arg(3);
    let dots = call.arg(4);
    let dot_names = call.arg(5);
    let subset = call.arg(6);
    let na_action = call.arg(7);
    let variable_count = length(variables);
    let dot_count = length(dots);
    if variables.kind() != SexpType::List && variables != nil() {
        return Err(call.error("invalid variables"));
    }
    if length(variable_names) != variable_count {
        return Err(call.error("number of variables != number of variable names"));
    }
    if length(dot_names) != dot_count {
        return Err(call.error("number of variables != number of variable names"));
    }
    let mut columns = Vec::with_capacity(variable_count + dot_count);
    let mut names = Vec::with_capacity(variable_count + dot_count);
    for index in 0..variable_count {
        columns.push(values::list_elt(variables, index));
        names.push(
            values::string_at(variable_names, index)
                .map(|name| name.into_owned())
                .unwrap_or_default(),
        );
    }
    for index in 0..dot_count {
        let value = values::list_elt(dots, index);
        if value == nil() {
            continue;
        }
        let name = values::string_at(dot_names, index)
            .map(|name| name.into_owned())
            .unwrap_or_default();
        columns.push(value);
        names.push(format!("({name})"));
    }
    for (column, name) in columns.iter().zip(&names) {
        match column.kind() {
            SexpType::Logical
            | SexpType::Integer
            | SexpType::Real
            | SexpType::Complex
            | SexpType::String
            | SexpType::Raw => {}
            SexpType::List
                if values::inherits(*column, "AsIs") || values::inherits(*column, "data.frame") => {
            }
            SexpType::Object => {}
            other => {
                return Err(call.error(format!(
                    "invalid type ({}) for variable '{name}'",
                    other.name()
                )));
            }
        }
    }
    // Without variables the frame keeps the data's rows, which only its row names describe,
    // compact (`c(NA, -n)`) or not.
    let rows = match columns.first() {
        Some(&column) => rows_of(column),
        None if row_names.kind() == SexpType::Integer
            && let [NA_INTEGER, count] = *row_names.slice::<i32>() =>
        {
            count.unsigned_abs() as usize
        }
        None => length(row_names),
    };
    for (column, name) in columns.iter().zip(&names) {
        if rows_of(*column) != rows {
            return Err(call.error(format!("variable lengths differ (found for '{name}')")));
        }
    }
    let data = values::list_vector(&columns)?;
    values::set_attr(data, symbols().names, values::string_vector(&names)?)?;
    let row_names = if length(row_names) == rows && rows > 0 {
        row_names
    } else {
        values::integer_vector(&[NA_INTEGER, rows as i32])?
    };
    values::set_attr(data, symbols().row_names, row_names)?;
    values::set_attr(data, symbols().class, values::scalar_string("data.frame")?)?;
    let data = if subset != nil() {
        let function = eval::find_fun(values::install("[.data.frame")?, values::base_namespace())?;
        let items = [
            (nil(), function),
            (nil(), data),
            (nil(), subset),
            (nil(), values::missing_arg()),
            (symbols().drop, values::scalar_bool(false)?),
        ];
        let subsetting = values::pairlist(&items, SexpType::Language)?;
        eval::eval(subsetting, call.env)?
    } else {
        data
    };
    if na_action == nil() {
        return Ok(data);
    }
    values::set_attr(data, values::install("terms")?, terms)?;
    let action = match values::first_string(na_action) {
        Some(name) if na_action.kind() == SexpType::String => values::install(&name)?,
        _ => na_action,
    };
    let action_call = values::call(action, &[data])?;
    let result = eval::eval(action_call, call.env)?;
    if result.kind() != SexpType::List || length(result) != length(data) {
        return Err(call.error("invalid result from na.action"));
    }
    for index in 0..length(result) {
        // An na.action that keeps every row hands back the frame's own vectors, which keep
        // their attributes, "ts" class included.
        let (original, column) = (
            values::list_elt(data, index),
            values::list_elt(result, index),
        );
        if original != column {
            copy_most_attributes_no_ts(original, column)?;
        }
    }
    Ok(result)
}

/// `modelmatrix(terms, frame)`: the design matrix of a model frame.
pub unsafe fn modelmatrix(call: &NativeCall<'_>) -> R<Sexp> {
    let terms = call.arg(0);
    let frame = call.arg(1);
    let attribute = |name: &str| -> R<Sexp> { Ok(values::get_attr(terms, values::install(name)?)) };
    let intercept = values::as_integer_scalar(attribute("intercept")?) != 0;
    let response = values::as_integer_scalar(attribute("response")?).max(0) as usize;
    let variables = attribute("variables")?;
    let factors_attribute = attribute("factors")?;
    let variable_count = length(variables).saturating_sub(1);
    let term_count = if length(factors_attribute) == 0 {
        0
    } else {
        matrix_shape(factors_attribute).1
    };
    let mut codes: Vec<i32> = if term_count == 0 {
        Vec::new()
    } else {
        values::ints(factors_attribute).to_vec()
    };
    if frame.kind() != SexpType::List || length(frame) < variable_count {
        return Err(call.error("invalid model frame"));
    }
    let variable_names: Vec<String> = (0..variable_count)
        .map(|index| variable_label(nth(variables, index + 1)))
        .collect();
    let rows = if length(frame) > 0 {
        rows_of(values::list_elt(frame, 0))
    } else {
        0
    };

    let mut kinds = Vec::with_capacity(variable_count);
    for (index, name) in variable_names.iter().enumerate() {
        let variable = values::list_elt(frame, index);
        if rows_of(variable) != rows {
            return Err(call.error(format!("variable lengths differ (found for '{name}')")));
        }
        let kind = if values::inherits(variable, "factor") {
            Variable::Factor {
                levels: length(values::get_attr(variable, symbols().levels)),
            }
        } else if variable.kind() == SexpType::Logical {
            Variable::Factor { levels: 2 }
        } else if matches!(variable.kind(), SexpType::Integer | SexpType::Real) {
            Variable::Numeric {
                columns: matrix_shape(variable).1,
            }
        } else if index + 1 == response {
            Variable::Numeric { columns: 0 }
        } else {
            return Err(call.error(format!(
                "invalid type ({}) for variable '{name}'",
                variable.kind().name()
            )));
        };
        kinds.push(kind);
    }
    if !intercept {
        'first: for term in 0..term_count {
            for variable in response..variable_count {
                if matches!(kinds[variable], Variable::Factor { levels } if levels > 0)
                    && codes[variable + term * variable_count] > 0
                {
                    codes[variable + term * variable_count] = 2;
                    break 'first;
                }
            }
        }
    }

    let contrasts_symbol = values::install("contrasts")?;
    let mut contrasts: Vec<Option<Sexp>> = vec![None; variable_count];
    let mut full_codings: Vec<Option<Sexp>> = vec![None; variable_count];
    for term in 0..term_count {
        for variable in 0..variable_count {
            let code = codes[variable + term * variable_count];
            if code == 0 || !matches!(kinds[variable], Variable::Factor { .. }) {
                continue;
            }
            let slot = if code == 1 {
                &mut contrasts[variable]
            } else {
                &mut full_codings[variable]
            };
            if slot.is_none() {
                let value = values::list_elt(frame, variable);
                let arguments = if code == 1 {
                    vec![value]
                } else {
                    vec![value, values::scalar_bool(false)?]
                };
                let contrast_call = values::call(contrasts_symbol, &arguments)?;
                let matrix = as_reals(eval::eval(contrast_call, call.env)?)?;
                *slot = Some(matrix);
            }
        }
    }

    let mut columns: Vec<Vec<f64>> = Vec::new();
    let mut column_names: Vec<String> = Vec::new();
    let mut assign: Vec<i32> = Vec::new();
    if intercept {
        columns.push(vec![1.0; rows]);
        column_names.push("(Intercept)".to_owned());
        assign.push(0);
    }
    for term in 0..term_count {
        let mut width = 1.0;
        for variable in 0..variable_count {
            let code = codes[variable + term * variable_count];
            width *= match (code, kinds[variable]) {
                (0, _) => 1.0,
                (_, Variable::Numeric { columns: count }) => count as f64,
                (1, Variable::Factor { .. }) => {
                    contrasts[variable].map_or(0.0, |coding| matrix_shape(coding).1 as f64)
                }
                (_, Variable::Factor { .. }) => {
                    full_codings[variable].map_or(0.0, |coding| matrix_shape(coding).1 as f64)
                }
            };
        }
        if width > i32::MAX as f64 {
            let width = crate::format::c_format(width, "g", 0, "", false);
            return Err(call.error(format!("term {} would require {width} columns", term + 1)));
        }
        let mut current: Vec<(Vec<f64>, String)> = vec![(vec![1.0; rows], String::new())];
        for variable in 0..variable_count {
            let code = codes[variable + term * variable_count];
            if code == 0 {
                continue;
            }
            let value = values::list_elt(frame, variable);
            let name = &variable_names[variable];
            let mut factor_columns: Vec<(Vec<f64>, String)> = Vec::new();
            match kinds[variable] {
                Variable::Numeric { columns: count } => {
                    let data = crate::stats::real_values(value)?;
                    let labels = column_labels(value, count)?;
                    for column in 0..count {
                        let label = if count == 1 {
                            name.clone()
                        } else {
                            format!("{name}{}", labels[column])
                        };
                        factor_columns
                            .push((data[column * rows..(column + 1) * rows].to_vec(), label));
                    }
                }
                Variable::Factor { levels } => {
                    let coding = if code == 1 {
                        contrasts[variable]
                    } else {
                        full_codings[variable]
                    };
                    let Some(coding) = coding else { continue };
                    let (coding_rows, coding_columns) = matrix_shape(coding);
                    if coding_rows != levels {
                        return Err(call.error(format!("invalid contrasts for variable '{name}'")));
                    }
                    let entries = values::reals(coding).to_vec();
                    let labels = column_labels(coding, coding_columns)?;
                    let level_codes: Vec<i32> = if value.kind() == SexpType::Logical {
                        values::ints(value)
                            .iter()
                            .map(|&entry| {
                                if entry == NA_INTEGER {
                                    NA_INTEGER
                                } else {
                                    entry + 1
                                }
                            })
                            .collect()
                    } else {
                        values::ints(value).to_vec()
                    };
                    for column in 0..coding_columns {
                        let data: Vec<f64> = level_codes
                            .iter()
                            .map(|&level| {
                                if level == NA_INTEGER || level < 1 || level as usize > levels {
                                    NA_REAL
                                } else {
                                    entries[level as usize - 1 + column * coding_rows]
                                }
                            })
                            .collect();
                        factor_columns.push((data, format!("{name}{}", labels[column])));
                    }
                }
            }
            let mut next = Vec::with_capacity(current.len() * factor_columns.len());
            for (factor_data, factor_label) in &factor_columns {
                for (data, label) in &current {
                    let product: Vec<f64> =
                        data.iter().zip(factor_data).map(|(a, b)| a * b).collect();
                    let joined = if label.is_empty() {
                        factor_label.clone()
                    } else {
                        format!("{label}:{factor_label}")
                    };
                    next.push((product, joined));
                }
            }
            current = next;
        }
        for (data, label) in current {
            columns.push(data);
            column_names.push(label);
            assign.push(term as i32 + 1);
        }
    }

    let mut data = Vec::with_capacity(rows * columns.len());
    for column in &columns {
        data.extend_from_slice(column);
    }
    let matrix = crate::stats::real_matrix(&data, rows, columns.len())?;
    let row_names = expanded_row_names(values::get_attr(frame, symbols().row_names), rows)?;
    let dimnames = values::list_vector(&[row_names, values::string_vector(&column_names)?])?;
    values::set_attr(matrix, symbols().dimnames, dimnames)?;
    values::set_attr(
        matrix,
        values::install("assign")?,
        values::integer_vector(&assign)?,
    )?;
    Ok(matrix)
}

/// `influence(qr, e, tol)`: the hat values of a QR fit and the residual standard deviations
/// with each case left out in turn.
pub unsafe fn influence(call: &NativeCall<'_>) -> R<Sexp> {
    let fit = call.arg(0);
    let residuals = as_reals(call.arg(1))?;
    let tolerance = values::as_real_scalar(call.arg(2));
    let qr = as_reals(values::list_get(fit, "qr"))?;
    let qraux = crate::stats::real_values(values::list_get(fit, "qraux"))?;
    let rank = values::as_integer_scalar(values::list_get(fit, "rank")).max(0) as usize;
    let (n, _) = matrix_shape(qr);
    let (_, outcomes) = matrix_shape(residuals);
    // The squared rows of the first `rank` columns of Q, from Q applied to unit vectors.
    let mut hat = vec![0.0; n];
    if rank > 0 {
        let units = values::alloc(SexpType::Real, n * rank)?;
        let data = values::reals_mut(units);
        data.fill(0.0);
        for column in 0..rank {
            data[column + column * n] = 1.0;
        }
        let rotated = values::alloc(SexpType::Real, n * rank)?;
        let integer = |value: usize| values::scalar_integer(value as i32);
        let qraux = values::real_vector(&qraux)?;
        let decomposition = values::duplicate(qr)?;
        linalg::dqrqy(&[
            decomposition,
            integer(n)?,
            integer(rank)?,
            qraux,
            units,
            integer(rank)?,
            rotated,
        ])?;
        let rotated = values::reals(rotated);
        for column in 0..rank {
            for (total, value) in hat.iter_mut().zip(&rotated[column * n..(column + 1) * n]) {
                *total += value * value;
            }
        }
    }
    for value in hat.iter_mut() {
        if *value >= 1.0 - tolerance {
            *value = 1.0;
        }
    }
    let errors = values::reals(residuals).to_vec();
    let denominator = n as f64 - rank as f64 - 1.0;
    let mut sigma = vec![0.0; n * outcomes];
    for outcome in 0..outcomes {
        let column = &errors[outcome * n..(outcome + 1) * n];
        let total: f64 = column.iter().map(|value| value * value).sum();
        for row in 0..n {
            let left_out = if hat[row] < 1.0 {
                total - column[row] * column[row] / (1.0 - hat[row])
            } else {
                total
            };
            sigma[row + outcome * n] = (left_out / denominator).sqrt();
        }
    }
    for value in hat.iter_mut() {
        if *value > 1.0 - tolerance {
            *value = 1.0;
        }
    }
    values::named_list(&[
        ("hat", values::real_vector(&hat)?),
        ("sigma", crate::stats::real_matrix(&sigma, n, outcomes)?),
    ])
}

/// A sorted set of variable indices.
type Term = Vec<usize>;

#[derive(Clone, Copy)]
enum Variable {
    Numeric { columns: usize },
    Factor { levels: usize },
}

struct Model {
    variables: Vec<Sexp>,
    intercept: bool,
    /// Inside the right operand of `-`, `1` removes the intercept and `0` restores it.
    parity: bool,
    /// The data columns `.` stands for.
    frame_names: Vec<String>,
    allow_dot_as_name: bool,
    call: Sexp,
}

impl Model {
    unsafe fn install(&mut self, variable: Sexp) -> R<usize> {
        let flags = IdenticalFlags::default();
        if let Some(index) = self
            .variables
            .iter()
            .position(|&known| identical(known, variable, &flags))
        {
            return Ok(index);
        }
        self.variables.push(variable);
        Ok(self.variables.len() - 1)
    }

    fn error(&self, message: &str) -> crate::unwind::Unwind {
        crate::unwind::error_in(self.call, message)
    }

    /// Installs the variables of `expression` in the order they appear, so that the variable
    /// list follows the formula even for terms that `-` later removes.
    unsafe fn extract(&mut self, expression: Sexp) -> R<()> {
        match expression.kind() {
            SexpType::Nil => Ok(()),
            SexpType::Symbol => {
                if is_dot(expression) && !self.frame_names.is_empty() {
                    for name in self.frame_names.clone() {
                        self.install(values::install(&name)?)?;
                    }
                    Ok(())
                } else if is_dot(expression) && !self.allow_dot_as_name {
                    Err(self.error("'.' in formula and no 'data' argument"))
                } else {
                    self.install(expression).map(|_| ())
                }
            }
            SexpType::Logical | SexpType::Integer | SexpType::Real
                if is_zero_or_one(expression) =>
            {
                Ok(())
            }
            SexpType::Language => {
                let operator = operator_name(expression);
                match operator.as_deref() {
                    Some("~") => Err(self.error("invalid model formula in ExtractVars")),
                    Some("+" | ":" | "*" | "/" | "%in%" | "-" | "(") => {
                        let mut arguments = expression.cdr();
                        while arguments != nil() {
                            self.extract(arguments.car())?;
                            arguments = arguments.cdr();
                        }
                        Ok(())
                    }
                    Some("^") => {
                        if length(expression) != 3
                            || !is_numeric_scalar(expression.cdr().cdr().car())
                        {
                            return Err(self.error("invalid power in formula"));
                        }
                        self.extract(expression.cdr().car())
                    }
                    _ => self.install(expression).map(|_| ()),
                }
            }
            _ => Err(self.error("invalid model formula in ExtractVars")),
        }
    }

    unsafe fn encode(&mut self, expression: Sexp) -> R<Vec<Term>> {
        match expression.kind() {
            SexpType::Nil => Ok(Vec::new()),
            SexpType::Logical | SexpType::Integer | SexpType::Real
                if is_zero_or_one(expression) =>
            {
                let one = values::as_real_scalar(expression) == 1.0;
                self.intercept = if one { self.parity } else { !self.parity };
                Ok(Vec::new())
            }
            SexpType::Symbol => {
                if is_dot(expression) && !self.frame_names.is_empty() {
                    let mut terms = Vec::new();
                    for name in self.frame_names.clone() {
                        terms.push(vec![self.install(values::install(&name)?)?]);
                    }
                    Ok(trim_repeats(terms))
                } else {
                    Ok(vec![vec![self.install(expression)?]])
                }
            }
            SexpType::Language => {
                let operator = operator_name(expression);
                let arguments = length(expression) - 1;
                let first = || expression.cdr().car();
                let second = || expression.cdr().cdr().car();
                match (operator.as_deref(), arguments) {
                    (Some("+"), 1) | (Some("("), 1) => self.encode(first()),
                    (Some("+"), 2) => {
                        let mut left = self.encode(first())?;
                        let right = self.encode(second())?;
                        left.extend(right);
                        Ok(trim_repeats(left))
                    }
                    (Some(":"), 2) => {
                        let left = self.encode(first())?;
                        let right = self.encode(second())?;
                        Ok(trim_repeats(interact(&left, &right)))
                    }
                    (Some("*"), 2) => {
                        let left = self.encode(first())?;
                        let right = self.encode(second())?;
                        let mut terms = left.clone();
                        terms.extend(right.iter().cloned());
                        terms.extend(interact(&left, &right));
                        Ok(trim_repeats(terms))
                    }
                    (Some("%in%"), 2) => {
                        let left = self.encode(first())?;
                        let right = self.encode(second())?;
                        let all = union_all(&right);
                        Ok(trim_repeats(
                            left.iter().map(|term| union(term, &all)).collect(),
                        ))
                    }
                    (Some("/"), 2) => {
                        let left = self.encode(first())?;
                        let right = self.encode(second())?;
                        let all = union_all(&left);
                        let mut terms = left;
                        terms.extend(right.iter().map(|term| union(&all, term)));
                        Ok(trim_repeats(terms))
                    }
                    (Some("^"), 2) => {
                        let power = values::as_real_scalar(second());
                        if !is_numeric_scalar(second()) || !(power >= 1.0) {
                            return Err(self.error("invalid power in formula"));
                        }
                        let left = self.encode(first())?;
                        let mut terms = left.clone();
                        for _ in 1..power as usize {
                            terms = trim_repeats(interact(&left, &terms));
                        }
                        Ok(terms)
                    }
                    (Some("-"), 1) => self.delete(Vec::new(), first()),
                    (Some("-"), 2) => {
                        let left = self.encode(first())?;
                        self.delete(left, second())
                    }
                    (Some("~"), _) => Err(self.error("invalid model formula in EncodeVars")),
                    _ => Ok(vec![vec![self.install(expression)?]]),
                }
            }
            _ => Err(self.error("invalid model formula in EncodeVars")),
        }
    }

    unsafe fn delete(&mut self, left: Vec<Term>, removed: Sexp) -> R<Vec<Term>> {
        self.parity = !self.parity;
        let right = self.encode(removed);
        self.parity = !self.parity;
        let right = right?;
        Ok(left
            .into_iter()
            .filter(|term| !right.contains(term))
            .collect())
    }
}

/// GNU R's code for variable `variable` in the term at `position`: 1 when the term with that
/// variable removed is empty or contained in an earlier term (so contrasts suffice), 2 when the
/// variable must be coded by indicators of all its levels.
fn term_code(terms: &[Term], position: usize, variable: usize) -> i32 {
    let remainder: Term = terms[position]
        .iter()
        .copied()
        .filter(|&other| other != variable)
        .collect();
    if remainder.is_empty() {
        return 1;
    }
    let covered = terms[..position]
        .iter()
        .any(|earlier| remainder.iter().all(|member| earlier.contains(member)));
    if covered { 1 } else { 2 }
}

fn union(left: &Term, right: &Term) -> Term {
    let mut result = left.clone();
    for &variable in right {
        if !result.contains(&variable) {
            result.push(variable);
        }
    }
    result.sort_unstable();
    result
}

fn union_all(terms: &[Term]) -> Term {
    terms.iter().fold(Vec::new(), |all, term| union(&all, term))
}

fn interact(left: &[Term], right: &[Term]) -> Vec<Term> {
    let mut result = Vec::with_capacity(left.len() * right.len());
    for l in left {
        for r in right {
            result.push(union(l, r));
        }
    }
    result
}

/// Drops empty terms and later repeats of a term.
fn trim_repeats(terms: Vec<Term>) -> Vec<Term> {
    let mut result: Vec<Term> = Vec::with_capacity(terms.len());
    for term in terms {
        if !term.is_empty() && !result.contains(&term) {
            result.push(term);
        }
    }
    result
}

unsafe fn operator_name(expression: Sexp) -> Option<String> {
    let head = expression.car();
    (head.kind() == SexpType::Symbol).then(|| values::symbol_name(head).into_owned())
}

pub(crate) unsafe fn is_call_to(expression: Sexp, name: &str) -> bool {
    expression.kind() == SexpType::Language
        && expression.car().kind() == SexpType::Symbol
        && values::symbol_name(expression.car()) == name
}

unsafe fn is_dot(expression: Sexp) -> bool {
    expression.kind() == SexpType::Symbol && values::symbol_name(expression) == "."
}

unsafe fn is_numeric_scalar(expression: Sexp) -> bool {
    matches!(
        expression.kind(),
        SexpType::Logical | SexpType::Integer | SexpType::Real
    ) && length(expression) == 1
}

unsafe fn is_zero_or_one(expression: Sexp) -> bool {
    is_numeric_scalar(expression)
        && matches!(values::as_real_scalar(expression), value if value == 0.0 || value == 1.0)
}

/// The symbols of a left-hand side, which `.` on the right excludes.
unsafe fn lhs_symbols(expression: Sexp, names: &mut Vec<String>) {
    match expression.kind() {
        SexpType::Symbol => names.push(values::symbol_name(expression).into_owned()),
        SexpType::Language => {
            let mut arguments = expression.cdr();
            while arguments != nil() {
                lhs_symbols(arguments.car(), names);
                arguments = arguments.cdr();
            }
        }
        _ => {}
    }
}

/// The right-hand side with `.` replaced by the sum of the data columns, parenthesized when
/// there is more than one.
unsafe fn expand_frame_dot(expression: Sexp, names: &[String]) -> R<Sexp> {
    if is_dot(expression) {
        let mut sum = values::install(&names[0])?;
        for name in &names[1..] {
            sum = values::call(symbols().plus, &[sum, values::install(name)?])?;
        }
        return if names.len() > 1 {
            values::call(symbols().paren, &[sum])
        } else {
            Ok(sum)
        };
    }
    if expression.kind() != SexpType::Language {
        return Ok(expression);
    }
    let copy = values::shallow_duplicate(expression)?;
    let mut cell = copy.cdr();
    while cell != nil() {
        let expanded = expand_frame_dot(cell.car(), names)?;
        crate::heap().set_car(cell, expanded);
        cell = cell.cdr();
    }
    Ok(copy)
}

/// `update.formula`'s substitution of `value` for `.` in `expression`. A substituted call is
/// parenthesized where its operator binds less tightly than the operator it becomes an operand
/// of, by the same table GNU R uses (which parenthesizes a little more than precedence needs).
unsafe fn expand_dots(expression: Sexp, value: Sexp) -> R<Sexp> {
    if expression.kind() == SexpType::Symbol {
        return if is_dot(expression) {
            values::duplicate(value)
        } else {
            Ok(expression)
        };
    }
    if expression.kind() != SexpType::Language {
        return Ok(expression);
    }
    let value_operator = if value.kind() == SexpType::Language {
        operator_name(value)
    } else {
        None
    };
    let wraps = |operators: &[&str]| {
        value_operator
            .as_deref()
            .is_some_and(|operator| operators.contains(&operator))
    };
    let (left, right): (&[&str], &[&str]) = match operator_name(expression).as_deref() {
        Some("+") => (&[], &[]),
        Some("-" | "*" | "/") => (&["+", "-"], &["+", "-"]),
        Some(":") => (&["+", "-", "*", "/"], &["+", "-"]),
        Some("^") => (&["+", "-", "*", "/", ":"], &[]),
        _ => {
            let mut cell = expression;
            while cell != nil() {
                let expanded = expand_dots(cell.car(), value)?;
                crate::heap().set_car(cell, expanded);
                cell = cell.cdr();
            }
            return Ok(expression);
        }
    };
    let mut cell = expression.cdr();
    let mut position = 0;
    while cell != nil() {
        let operand = cell.car();
        let parenthesize = is_dot(operand) && wraps(if position == 0 { left } else { right });
        let mut expanded = expand_dots(operand, value)?;
        if parenthesize {
            expanded = values::call(symbols().paren, &[expanded])?;
        }
        crate::heap().set_car(cell, expanded);
        cell = cell.cdr();
        position += 1;
    }
    Ok(expression)
}

/// A variable's name as `terms` labels it: its one-line deparse.
pub(crate) unsafe fn variable_label(variable: Sexp) -> String {
    deparse_lines(variable, 500, options::SIMPLE).concat()
}

unsafe fn nth(list: Sexp, index: usize) -> Sexp {
    let mut cell = list;
    for _ in 0..index {
        cell = cell.cdr();
    }
    cell.car()
}

/// The number of rows of a model frame column: a matrix's or data frame's rows, else its
/// length.
unsafe fn rows_of(x: Sexp) -> usize {
    if values::inherits(x, "data.frame") {
        let row_names = values::get_attr(x, symbols().row_names);
        return compact_row_count(row_names).unwrap_or_else(|| length(row_names));
    }
    match values::dims(x) {
        Some(dims) if !dims.is_empty() => dims[0],
        _ => length(x),
    }
}

pub(crate) unsafe fn compact_row_count(row_names: Sexp) -> Option<usize> {
    if row_names.kind() == SexpType::Integer
        && length(row_names) == 2
        && values::ints(row_names)[0] == NA_INTEGER
    {
        Some(values::ints(row_names)[1].unsigned_abs() as usize)
    } else {
        None
    }
}

/// Row names as a character vector, expanding GNU R's compact `c(NA, n)` form.
unsafe fn expanded_row_names(row_names: Sexp, rows: usize) -> R<Sexp> {
    if row_names == nil() || compact_row_count(row_names).is_some() {
        let names: Vec<String> = (1..=rows).map(|row| row.to_string()).collect();
        return values::string_vector(&names);
    }
    if row_names.kind() == SexpType::String {
        return Ok(row_names);
    }
    crate::coerce::coerce_vector(row_names, SexpType::String)
}

/// The column labels of a matrix: its column names, or the column numbers.
unsafe fn column_labels(x: Sexp, count: usize) -> R<Vec<String>> {
    let dimnames = values::get_attr(x, symbols().dimnames);
    if dimnames.kind() == SexpType::List && length(dimnames) == 2 {
        let names = values::list_elt(dimnames, 1);
        if names.kind() == SexpType::String && length(names) == count {
            return Ok(values::strings(names)
                .into_iter()
                .map(|name| name.unwrap_or_else(|| "NA".to_owned()))
                .collect());
        }
    }
    Ok((1..=count).map(|column| column.to_string()).collect())
}

/// Copies the attributes `na.action` may have dropped from a model frame column: all but the
/// names, dimensions and time series properties.
unsafe fn copy_most_attributes_no_ts(from: Sexp, to: Sexp) -> R<()> {
    let skipped = [
        symbols().names,
        symbols().dim,
        symbols().dimnames,
        symbols().tsp,
    ];
    for (tag, value) in values::pairlist_items(from.attrib()) {
        if skipped.contains(&tag) {
            continue;
        }
        if tag == symbols().class {
            let classes: Vec<String> = values::strings(value)
                .into_iter()
                .flatten()
                .filter(|class| class != "ts")
                .collect();
            if classes.is_empty() {
                continue;
            }
            values::set_attr(to, tag, values::string_vector(&classes)?)?;
            continue;
        }
        values::set_attr(to, tag, value)?;
    }
    Ok(())
}

// Nonlinear least squares.

/// `numeric_deriv(expr, theta, rho, dir, eps, central)`: the value of the model `expr` in
/// `rho`, with the forward (or central) difference gradient with respect to the parameters
/// named in `theta` as its `gradient` attribute.
pub unsafe fn numeric_deriv(call: &NativeCall<'_>) -> R<Sexp> {
    let expression = call.arg(0);
    let theta = call.arg(1);
    let env = call.arg(2);
    let directions = crate::stats::real_values(call.arg(3))?;
    let epsilon = values::as_real_scalar(call.arg(4));
    let central = values::as_integer_scalar(call.arg(5)) == 1;
    if theta.kind() != SexpType::String {
        return Err(call.error("'theta' should be of type character"));
    }
    if env.kind() != SexpType::Environment {
        return Err(call.error("use of NULL environment is defunct"));
    }
    let names = values::strings(theta);
    if directions.len() != names.len() {
        return Err(call.error("'dir' is not a numeric vector of the correct length"));
    }
    let evaluate = || -> R<Sexp> {
        let value = eval::eval(expression, env)?;
        let value = if value.kind() == SexpType::Real {
            values::duplicate(value)?
        } else {
            crate::coerce::coerce_vector(value, SexpType::Real)?
        };
        Ok(value)
    };
    let answer = evaluate()?;
    if values::reals(answer).iter().any(|value| !value.is_finite()) {
        return Err(call.error("Missing value or an infinity produced when evaluating the model"));
    }
    let mut parameters = Vec::with_capacity(names.len());
    for name in &names {
        let name = name.clone().unwrap_or_default();
        let symbol = values::install(&name)?;
        let value = eval::find_var(symbol, env)?;
        let value = eval::value_of(value)?;
        if value.kind() == SexpType::Integer {
            return Err(call.error(format!("variable '{name}' is integer, not numeric")));
        }
        if value.kind() != SexpType::Real {
            return Err(call.error(format!("variable '{name}' is not numeric")));
        }
        // The parameter is perturbed in place, so it must not be shared.
        let own = values::duplicate(value)?;
        eval::define(symbol, own, env)?;
        parameters.push(own);
    }
    let rows = length(answer);
    let total: usize = parameters.iter().map(|&parameter| length(parameter)).sum();
    let mut gradient = vec![0.0; rows * total];
    let base = values::reals(answer).to_vec();
    let mut column = 0;
    for (index, &parameter) in parameters.iter().enumerate() {
        for j in 0..length(parameter) {
            let original = values::reals(parameter)[j];
            let delta = if original == 0.0 {
                epsilon
            } else {
                original.abs() * epsilon
            };
            values::reals_mut(parameter)[j] = original + directions[index] * delta;
            let forward = crate::scope(|| Ok(ValueVector(values::reals(evaluate()?).to_vec())))?.0;
            if central {
                values::reals_mut(parameter)[j] = original - directions[index] * delta;
                let backward =
                    crate::scope(|| Ok(ValueVector(values::reals(evaluate()?).to_vec())))?.0;
                for k in 0..rows {
                    if !forward[k].is_finite() || !backward[k].is_finite() {
                        return Err(call.error(
                            "Missing value or an infinity produced when evaluating the model",
                        ));
                    }
                    gradient[column * rows + k] =
                        directions[index] * (forward[k] - backward[k]) / (2.0 * delta);
                }
            } else {
                for k in 0..rows {
                    if !forward[k].is_finite() {
                        return Err(call.error(
                            "Missing value or an infinity produced when evaluating the model",
                        ));
                    }
                    gradient[column * rows + k] =
                        directions[index] * (forward[k] - base[k]) / delta;
                }
            }
            values::reals_mut(parameter)[j] = original;
            column += 1;
        }
    }
    let matrix = crate::stats::real_matrix(&gradient, rows, total)?;
    values::set_attr(answer, values::install("gradient")?, matrix)?;
    Ok(answer)
}

/// A vector of numbers, which holds no handles.
struct ValueVector(Vec<f64>);

impl crate::unwind::Rooted for ValueVector {}

/// `nls_iter(m, control, trace)`: Gauss-Newton iterations of an `nlsModel`, halving the step
/// until the deviance does not increase.
pub unsafe fn nls_iter(call: &NativeCall<'_>) -> R<Sexp> {
    let model = call.arg(0);
    let control = call.arg(1);
    let trace_on = values::as_logical_scalar(call.arg(2)) == 1;
    if control.kind() != SexpType::List {
        return Err(call.error("'control' must be a list"));
    }
    if model.kind() != SexpType::List {
        return Err(call.error("'m' must be a list"));
    }
    let setting = |name: &str| -> R<Sexp> {
        let value = values::list_get(control, name);
        if value == nil() {
            return Err(call.error(format!("'control${name}' absent")));
        }
        Ok(value)
    };
    let max_iterations = values::as_integer_scalar(setting("maxiter")?);
    let tolerance = values::as_real_scalar(setting("tol")?);
    let min_factor = values::as_real_scalar(setting("minFactor")?);
    let warn_only = values::as_logical_scalar(setting("warnOnly")?) == 1;
    let print_eval = values::as_logical_scalar(setting("printEval")?) == 1;
    let component = |name: &str| -> R<Sexp> {
        let value = values::list_get(model, name);
        if value == nil() {
            return Err(call.error(format!("'m${name}()' absent")));
        }
        values::call(value, &[])
    };
    let convergence = component("conv")?;
    let increment = component("incr")?;
    let deviance_call = component("deviance")?;
    let trace = component("trace")?;
    let set_parameters = values::list_get(model, "setPars");
    if set_parameters == nil() {
        return Err(call.error("'m$setPars()' absent"));
    }
    let get_parameters = component("getPars")?;
    let global = values::global_env();
    let scalar = |expression: Sexp| -> R<f64> {
        crate::scope(|| Ok(values::as_real_scalar(eval::eval(expression, global)?)))
    };
    let mut parameters = crate::stats::real_values(eval::eval(get_parameters, global)?)?;
    let mut deviance = scalar(deviance_call)?;
    if trace_on {
        eval::eval(trace, global)?;
    }
    let mut factor = 1.0_f64;
    let mut converged = false;
    let mut total_evaluations = 1;
    let mut criterion = -1.0;
    let mut iteration = 0;
    let finish = |message: String, code: i32, iteration: i32, criterion: f64| -> R<Sexp> {
        values::named_list(&[
            ("isConv", values::scalar_bool(code == 0)?),
            ("finIter", values::scalar_integer(iteration)?),
            ("finTol", values::scalar_real(criterion)?),
            ("stopCode", values::scalar_integer(code)?),
            ("stopMessage", values::scalar_string(&message)?),
        ])
    };
    let fail = |message: String, code: i32, iteration: i32, criterion: f64| -> R<Sexp> {
        if warn_only {
            call.warning(message.clone())?;
            finish(message, code, iteration, criterion)
        } else {
            Err(call.error(message))
        }
    };
    while iteration < max_iterations {
        criterion = scalar(convergence)?;
        if criterion <= tolerance {
            converged = true;
            break;
        }
        let step = crate::stats::real_values(eval::eval(increment, global)?)?;
        let mut evaluations = if print_eval { 1 } else { -1 };
        while factor >= min_factor {
            if print_eval {
                crate::write_console(
                    format!(
                        "  It. {:3}, fac= {:>11}, eval (no.,total): ({:2},{:3}):",
                        iteration + 1,
                        crate::format::c_general(factor, 6, false),
                        evaluations,
                        total_evaluations
                    )
                    .as_bytes(),
                    false,
                );
                evaluations += 1;
                total_evaluations += 1;
            }
            let trial: Vec<f64> = parameters
                .iter()
                .zip(&step)
                .map(|(value, change)| value + factor * change)
                .collect();
            let singular = crate::scope(|| {
                let argument = values::real_vector(&trial)?;
                let expression = values::call(set_parameters, &[argument])?;
                Ok(values::as_logical_scalar(eval::eval(expression, global)?) == 1)
            })?;
            if singular {
                return fail("singular gradient".to_owned(), 1, iteration, criterion);
            }
            let new_deviance = scalar(deviance_call)?;
            if print_eval {
                crate::write_console(
                    format!(
                        " new dev = {}\n",
                        crate::format::c_general(new_deviance, 6, false)
                    )
                    .as_bytes(),
                    false,
                );
            }
            if new_deviance <= deviance {
                deviance = new_deviance;
                parameters = trial;
                factor = (2.0 * factor).min(1.0);
                break;
            }
            factor /= 2.0;
        }
        if trace_on {
            eval::eval(trace, global)?;
        }
        if factor < min_factor {
            let message = format!(
                "step factor {} reduced below 'minFactor' of {}",
                crate::format::c_general(factor, 6, false),
                crate::format::c_general(min_factor, 6, false)
            );
            return fail(message, 2, iteration, criterion);
        }
        iteration += 1;
    }
    if !converged {
        return fail(
            format!("number of iterations exceeded maximum of {max_iterations}"),
            3,
            iteration,
            criterion,
        );
    }
    finish("converged".to_owned(), 0, iteration, criterion)
}
