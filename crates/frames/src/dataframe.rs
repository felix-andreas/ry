//! The Polars-style `DataFrame` API: column expressions built with `Expr.col("x")` and the verbs
//! `DataFrame.select`, `DataFrame.filter`, `DataFrame.group_by` with `GroupBy.agg`, and the rest,
//! called by qualified name and chained with `|>`.
//!
//! An expression, a `LazyFrame` and a `GroupBy` are external pointers that own a Rust value
//! ([`Object`]) and carry an R class, so R code passes them around like any other value. Arithmetic
//! and comparisons on an expression reach `Ops.Expr`, which builds the larger expression instead
//! of computing anything. A verb on a `DataFrame` runs at once as a one-step [`Plan`]; the same verb
//! on a `LazyFrame` appends the step, and `LazyFrame.collect` optimizes and runs the whole plan.

use {
    crate::frame::{self, is_frame, table_of, table_work, vector_literal},
    heap::object::{Sexp, SexpType},
    interp::{
        builtins::{BuiltinDef, Call, builtin},
        conditions,
        unwind::{R, error_in},
        values::{self, install, is_nil, length, nil, scalar_string, set_attr, symbols},
    },
    std::{cell::Cell, ffi::c_void},
    table::{
        ColumnType, Error as TableError, Schema,
        lazy::{self, How, Plan, Source, Step, Strategy},
        ops,
        polars::prelude::PlSmallStr,
        query::{Aggregate, BinaryOp, Expr, Function, Output, SortKey, UnaryOp, Warning},
    },
};

pub const BUILTINS: &[BuiltinDef] = &[
    builtin(".expr_column", 1, table_work!(do_column)),
    builtin(".expr_literal", 1, table_work!(do_literal)).accepting_native(),
    builtin(".expr_operator", 4, table_work!(do_operator)).accepting_native(),
    builtin(".expr_aggregate", 3, table_work!(do_aggregate)),
    builtin(".expr_len", 0, table_work!(do_len)),
    builtin(".expr_alias", 2, table_work!(do_alias)),
    builtin(".expr_if_else", 3, table_work!(do_if_else)),
    builtin(".expr_cor", 2, table_work!(do_cor)),
    builtin(".expr_head", 3, table_work!(do_head_expr)),
    builtin(".expr_format", 1, table_work!(do_format)),
    builtin(".expr_over", 2, table_work!(do_over)),
    builtin(".expr_rank", 3, table_work!(do_rank)),
    builtin(".expr_call", 4, table_work!(do_call)),
    builtin(".df_select", 2, table_work!(do_select)),
    builtin(".df_with_columns", 2, table_work!(do_with_columns)),
    builtin(".df_filter", 2, table_work!(do_filter)),
    builtin(".df_group_by", 2, table_work!(do_group_by)),
    builtin(".df_agg", 2, table_work!(do_agg)),
    builtin(".df_sort", 3, table_work!(do_sort)),
    builtin(".df_head", 3, table_work!(do_head)),
    builtin(".df_tail", 3, table_work!(do_tail)),
    builtin(".df_join", 8, table_work!(do_join)),
    builtin(".df_lazy", 1, table_work!(do_lazy)),
    builtin(".df_collect", 2, table_work!(do_collect)),
    builtin(".df_explain", 2, table_work!(do_explain)),
    builtin(".df_scan", 2, table_work!(do_scan)),
    builtin(".df_pivot", 6, table_work!(do_pivot)),
    builtin(".df_concat", 3, table_work!(do_concat)),
    builtin(".df_unique", 4, table_work!(do_unique)),
    builtin(".df_duplicated", 3, table_work!(do_duplicated)),
    builtin(".df_drop", 2, table_work!(do_drop)),
    builtin(".df_rename", 2, table_work!(do_rename)),
    builtin(".df_with_row_index", 4, table_work!(do_with_row_index)),
    builtin(".df_unpivot", 6, table_work!(do_unpivot)),
];

/// The API, bound in the `ry` namespace and attached with the rest of `package:ry`. Each verb
/// passes `sys.call()` so an error names the call the user wrote, not the internal one. A verb
/// that takes columns through `...` has no formal before them and takes its table (or group, or
/// expression) as the first argument by position, so no column name, not even `df`, can match a
/// formal (`.agents/memory/decisions/frames-table.md`).
pub const PACKAGE: &str = r#"
Expr.col <- function(name) .expr_column(name)
Expr.lit <- function(value) .expr_literal(value)
Expr.alias <- function(expr, name) .expr_alias(expr, name)
Expr.sum <- function(expr, na_rm = TRUE) .expr_aggregate(expr, "sum", na_rm)
Expr.mean <- function(expr, na_rm = TRUE) .expr_aggregate(expr, "mean", na_rm)
Expr.min <- function(expr, na_rm = TRUE) .expr_aggregate(expr, "min", na_rm)
Expr.max <- function(expr, na_rm = TRUE) .expr_aggregate(expr, "max", na_rm)
Expr.first <- function(expr) .expr_aggregate(expr, "first", FALSE)
Expr.last <- function(expr) .expr_aggregate(expr, "last", FALSE)
Expr.median <- function(expr, na_rm = TRUE) .expr_aggregate(expr, "median", na_rm)
Expr.std <- function(expr, na_rm = TRUE) .expr_aggregate(expr, "std", na_rm)
Expr.var <- function(expr, na_rm = TRUE) .expr_aggregate(expr, "var", na_rm)
Expr.count <- function(expr) .expr_aggregate(expr, "count", FALSE)
Expr.head <- function(expr, n = 10L) .expr_head(expr, n, sys.call())
Expr.is_null <- function(expr) .expr_operator("is_null", expr, NULL, TRUE)
Expr.len <- function() .expr_len()
Expr.if_else <- function(test, yes, no) .expr_if_else(test, yes, no)
Expr.cor <- function(first, second) .expr_cor(first, second)
Expr.over <- function(...) .expr_over(list(...), sys.call())
Expr.rank <- function(expr, descending = FALSE) .expr_rank(expr, descending, sys.call())
Expr.is_in <- function(expr, values) .expr_call("is_in", list(expr, values), NULL, sys.call())
Expr.n_unique <- function(expr) .expr_aggregate(expr, "n_unique", FALSE)
Expr.str_contains <- function(expr, pattern, literal = FALSE) .expr_call("str_contains", list(expr, pattern), list(literal), sys.call())
Expr.str_starts_with <- function(expr, prefix) .expr_call("str_starts_with", list(expr, prefix), NULL, sys.call())
Expr.str_ends_with <- function(expr, suffix) .expr_call("str_ends_with", list(expr, suffix), NULL, sys.call())
Expr.str_replace <- function(expr, pattern, replacement, literal = FALSE) {
  .expr_call("str_replace", list(expr, pattern, replacement), list(literal), sys.call())
}
Expr.str_replace_all <- function(expr, pattern, replacement, literal = FALSE) {
  .expr_call("str_replace_all", list(expr, pattern, replacement), list(literal), sys.call())
}
Expr.str_slice <- function(expr, start, length = NULL) .expr_call("str_slice", list(expr), list(start, length), sys.call())
Expr.str_to_uppercase <- function(expr) .expr_call("str_to_uppercase", list(expr), NULL, sys.call())
Expr.str_to_lowercase <- function(expr) .expr_call("str_to_lowercase", list(expr), NULL, sys.call())
Expr.str_to_date <- function(expr, format = "%Y-%m-%d") .expr_call("str_to_date", list(expr), list(format), sys.call())
Expr.concat_str <- function(..., separator = "") .expr_call("concat_str", list(...), list(separator), sys.call())
Expr.dt_year <- function(expr) .expr_call("dt_year", list(expr), NULL, sys.call())
Expr.dt_month <- function(expr) .expr_call("dt_month", list(expr), NULL, sys.call())
Expr.dt_day <- function(expr) .expr_call("dt_day", list(expr), NULL, sys.call())
Expr.dt_offset_by <- function(expr, by) .expr_call("dt_offset_by", list(expr), list(by), sys.call())
Expr.dt_to_string <- function(expr, format) .expr_call("dt_to_string", list(expr), list(format), sys.call())
Expr.fill_null <- function(expr, value) .expr_call("fill_null", list(expr, value), NULL, sys.call())
Expr.coalesce <- function(...) .expr_call("coalesce", list(...), NULL, sys.call())
Expr.cast <- function(expr, type) .expr_call("cast", list(expr), list(type), sys.call())
Expr.shift <- function(expr, n = 1L) .expr_call("shift", list(expr), list(n), sys.call())
Expr.cum_sum <- function(expr) .expr_call("cum_sum", list(expr), NULL, sys.call())
DataFrame.height <- function(df) dim(df)[[1L]]
DataFrame.width <- function(df) dim(df)[[2L]]
DataFrame.columns <- function(df) names(df)
DataFrame.select <- function(...) .df_select(list(...), sys.call())
DataFrame.with_columns <- function(...) .df_with_columns(list(...), sys.call())
DataFrame.filter <- function(...) .df_filter(list(...), sys.call())
DataFrame.group_by <- function(...) .df_group_by(list(...), sys.call())
DataFrame.sort <- function(..., descending = FALSE) .df_sort(list(...), descending, sys.call())
DataFrame.head <- function(df, n = 5L) .df_head(df, n, sys.call())
DataFrame.tail <- function(df, n = 5L) .df_tail(df, n, sys.call())
DataFrame.join <- function(df, other, on = NULL, how = "inner", left_on = on, right_on = on, suffix = "_right") {
  .df_join(df, other, left_on, right_on, how, suffix, NULL, sys.call())
}
DataFrame.join_asof <- function(df, other, on, by = NULL, strategy = "backward", suffix = "_right") {
  .df_join(df, other, c(by, on), c(by, on), "asof", suffix, strategy, sys.call())
}
DataFrame.lazy <- function(df) .df_lazy(df)
DataFrame.concat <- function(..., how = "vertical") .df_concat(list(...), how, sys.call())
DataFrame.unique <- function(df, subset = NULL, keep = "first") .df_unique(df, subset, keep, sys.call())
DataFrame.pivot <- function(df, on, index, values, agg = NULL) .df_pivot(df, on, index, values, agg, sys.call())
DataFrame.unpivot <- function(df, on = NULL, index = NULL, variable_name = "variable", value_name = "value") {
  .df_unpivot(df, on, index, variable_name, value_name, sys.call())
}
DataFrame.drop <- function(...) .df_drop(list(...), sys.call())
DataFrame.rename <- function(...) .df_rename(list(...), sys.call())
DataFrame.with_row_index <- function(df, name = "index", offset = 1L) .df_with_row_index(df, name, offset, sys.call())
GroupBy.agg <- function(...) .df_agg(list(...), sys.call())
LazyFrame.scan_csv <- function(path) .df_scan(path, "csv")
LazyFrame.scan_parquet <- function(path) .df_scan(path, "parquet")
LazyFrame.select <- DataFrame.select
LazyFrame.with_columns <- DataFrame.with_columns
LazyFrame.filter <- DataFrame.filter
LazyFrame.group_by <- DataFrame.group_by
LazyFrame.sort <- DataFrame.sort
LazyFrame.head <- DataFrame.head
LazyFrame.tail <- DataFrame.tail
LazyFrame.join <- DataFrame.join
LazyFrame.join_asof <- DataFrame.join_asof
LazyFrame.pivot <- DataFrame.pivot
LazyFrame.unique <- DataFrame.unique
LazyFrame.unpivot <- DataFrame.unpivot
LazyFrame.drop <- DataFrame.drop
LazyFrame.rename <- DataFrame.rename
LazyFrame.with_row_index <- DataFrame.with_row_index
LazyFrame.collect <- function(lf) .df_collect(lf, sys.call())
LazyFrame.explain <- function(lf) {
  plan <- .df_explain(lf, sys.call())
  cat(plan)
  invisible(plan)
}
LazyGroupBy.agg <- GroupBy.agg
"#;

/// The S3 methods of the API's classes, registered in base's methods table as an installed
/// package's `S3method()` directives are, because S3 lookup from the global environment skips the
/// attached packages.
pub const METHODS: &str = r#"
local({
  ns <- asNamespace("ry")
  methods <- list(
    Ops.Expr = function(e1, e2) {
      if (missing(e2)) .expr_operator(.Generic, e1, NULL, TRUE) else .expr_operator(.Generic, e1, e2, FALSE)
    },
    # An expression wins over the other operand's class, so `Expr.col("d") >= as.Date(...)` builds an
    # expression instead of failing on conflicting `Ops.Expr` and `Ops.Date` methods.
    chooseOpsMethod.Expr = function(x, y, mx, my, cl, reverse) TRUE,
    format.Expr = function(x, ...) .expr_format(x),
    # As for a data.frame, a list or data.frame argument is a frame of rows (matched to the
    # DataFrame's columns by position when it has no names) and a NULL one is skipped.
    rbind.DataFrame = function(..., deparse.level = 1) {
      items <- Filter(Negate(is.null), list(...))
      columns <- NULL
      for (item in items) if (inherits(item, "DataFrame")) {
        columns <- names(item)
        break
      }
      items <- lapply(items, function(item) {
        if (!is.list(item)) return(item)
        if (is.null(names(item)) && length(item) == length(columns)) names(item) <- columns
        as.DataFrame(item)
      })
      DataFrame.concat(items, how = "vertical")
    },
    # As for a data.frame, an unnamed vector argument is a column named after its expression.
    cbind.DataFrame = function(..., deparse.level = 1) {
      items <- list(...)
      labels <- vapply(as.list(substitute(list(...)))[-1L], function(e) paste(deparse(e), collapse = " "), "")
      given <- names(items)
      if (is.null(given)) given <- character(length(items))
      for (index in seq_along(items)) {
        item <- items[[index]]
        if (is.list(item)) items[[index]] <- as.DataFrame(item)
        else if (!inherits(item, "DataFrame") && !nzchar(given[[index]])) given[[index]] <- labels[[index]]
      }
      kept <- !vapply(items, is.null, NA)
      items <- items[kept]
      names(items) <- given[kept]
      DataFrame.concat(items, how = "horizontal")
    },
    unique.DataFrame = function(x, incomparables = FALSE, fromLast = FALSE, ...) {
      if (!isFALSE(incomparables)) stop("`incomparables` is not supported for a DataFrame", call. = FALSE)
      DataFrame.unique(x, keep = if (isTRUE(fromLast)) "last" else "first")
    },
    duplicated.DataFrame = function(x, incomparables = FALSE, fromLast = FALSE, ...) {
      if (!isFALSE(incomparables)) stop("`incomparables` is not supported for a DataFrame", call. = FALSE)
      .df_duplicated(x, isTRUE(fromLast), sys.call())
    },
    # `%in%` is not generic, so an expression on its left stops in `match()` with a pointer to the
    # expression's own test instead of an error about coercing a pointer.
    mtfrm.Expr = function(x) stop("`%in%` does not build an expression; write `Expr.is_in(x, values)`", call. = FALSE),
    print.Expr = function(x, ...) {
      cat(.expr_format(x), "\n", sep = "")
      invisible(x)
    },
    print.LazyFrame = function(x, ...) {
      cat("<LazyFrame>\n", .df_explain(x, sys.call()), sep = "")
      invisible(x)
    },
    print.GroupBy = function(x, ...) {
      cat("<GroupBy>\n")
      invisible(x)
    },
    print.LazyGroupBy = function(x, ...) {
      cat("<LazyGroupBy>\n")
      invisible(x)
    }
  )
  for (name in names(methods)) {
    method <- methods[[name]]
    environment(method) <- ns
    split <- regexpr(".", name, fixed = TRUE)
    registerS3method(substr(name, 1L, split - 1L), substring(name, split + 1L), method, envir = ns)
  }
})
"#;

/// A value of the API that is not a `DataFrame`.
enum Object {
    Expr(Named),
    Lazy(Plan),
    Group {
        plan: Plan,
        keys: Vec<(PlSmallStr, Expr)>,
        lazy: bool,
    },
}

/// An expression and the output name `Expr.alias` gave it.
#[derive(Clone)]
struct Named {
    alias: Option<PlSmallStr>,
    expr: Expr,
}

impl Named {
    fn output(self) -> (PlSmallStr, Expr) {
        let name = self.alias.unwrap_or_else(|| lazy::output_name(&self.expr));
        (name, self.expr)
    }
}

thread_local! {
    static TAG: Cell<Option<Sexp>> = const { Cell::new(None) };
}

unsafe fn object_tag() -> R<Sexp> {
    if let Some(tag) = TAG.with(Cell::get) {
        return Ok(tag);
    }
    let tag = install("ry.object")?;
    TAG.with(|cell| cell.set(Some(tag)));
    Ok(tag)
}

unsafe fn wrap(object: Object) -> R<Sexp> {
    let class = match &object {
        Object::Expr(_) => "Expr",
        Object::Lazy(_) => "LazyFrame",
        Object::Group { lazy: false, .. } => "GroupBy",
        Object::Group { lazy: true, .. } => "LazyGroupBy",
    };
    let address = Box::into_raw(Box::new(object)).cast::<c_void>();
    let heap = interp::heap();
    let value = match heap.alloc_external_ptr(address, object_tag()?, nil()) {
        Ok(value) => value,
        Err(failure) => {
            drop(Box::from_raw(address.cast::<Object>()));
            return Err(failure.into());
        }
    };
    heap.root(value);
    heap.alloc_weak_ref(value, nil(), nil(), finalize as *const () as usize, false)?;
    set_attr(value, symbols().class, scalar_string(class)?)?;
    Ok(value)
}

unsafe extern "C" fn finalize(value: Sexp) {
    let address = value.external_address();
    if !address.is_null() {
        value.set_external_address(std::ptr::null_mut());
        drop(Box::from_raw(address.cast::<Object>()));
    }
}

unsafe fn object_of<'a>(value: Sexp) -> Option<&'a Object> {
    let tagged = value.kind() == SexpType::ExternalPtr
        && TAG
            .with(Cell::get)
            .is_some_and(|tag| value.external_tag() == tag)
        && !value.external_address().is_null();
    tagged.then(|| &*value.external_address().cast::<Object>())
}

/// Whether `value` is one of the API's own values: a `DataFrame`, a `LazyFrame`, a `GroupBy` or an
/// expression. Typed code holds one without looking inside it.
pub unsafe fn is_table_value(value: Sexp) -> bool {
    is_frame(value) || object_of(value).is_some()
}

/// What an R value is to the API, for error messages.
unsafe fn describe(value: Sexp) -> String {
    if is_frame(value) {
        return "a DataFrame".to_string();
    }
    match object_of(value) {
        Some(Object::Expr(_)) => "an expression".to_string(),
        Some(Object::Lazy(_)) => "a LazyFrame".to_string(),
        Some(Object::Group { .. }) => "a GroupBy".to_string(),
        None if is_nil(value) => "NULL".to_string(),
        None => {
            let kind = value.kind().name();
            let article = if kind.starts_with(['a', 'e', 'i', 'o', 'u']) {
                "an"
            } else {
                "a"
            };
            format!("{article} {kind} vector")
        }
    }
}

/// An operand: an expression, or an R vector as a literal. A single value broadcasts to every
/// row, and a longer vector is a column of its own, one value per row.
unsafe fn operand(value: Sexp, call: Sexp) -> R<Expr> {
    if let Some(Object::Expr(named)) = object_of(value) {
        return Ok(named.expr.clone());
    }
    vector_literal(value)?.ok_or_else(|| {
        error_in(
            call,
            format!(
                "an expression combines with expressions and vectors, not {}",
                describe(value)
            ),
        )
    })
}

unsafe fn expression(value: Sexp, call: Sexp) -> R<Named> {
    match object_of(value) {
        Some(Object::Expr(named)) => Ok(named.clone()),
        _ => Err(error_in(
            call,
            format!(
                "expected an expression such as `Expr.col(\"x\")`, got {}",
                describe(value)
            ),
        )),
    }
}

unsafe fn expr(expr: Expr) -> R<Sexp> {
    wrap(Object::Expr(Named { alias: None, expr }))
}

unsafe fn do_column(call: &Call<'_>) -> R<Sexp> {
    let name = values::require_str(call.arg(0), "name")?;
    expr(Expr::Column(name.into()))
}

unsafe fn do_literal(call: &Call<'_>) -> R<Sexp> {
    expr(operand(call.arg(0), call.call)?)
}

unsafe fn do_operator(call: &Call<'_>) -> R<Sexp> {
    let operator = values::require_str(call.arg(0), "operator")?;
    let left = operand(call.arg(1), call.call)?;
    if values::require_flag(call.arg(3), "unary")? {
        let op = match operator.as_ref() {
            "-" => UnaryOp::Negate,
            "!" => UnaryOp::Not,
            "is_null" => UnaryOp::IsNa,
            "+" => return expr(left),
            other => {
                return Err(error_in(
                    call.call,
                    format!("`{other}` does not apply to one expression"),
                ));
            }
        };
        return expr(Expr::Unary(op, Box::new(left)));
    }
    let right = operand(call.arg(2), call.call)?;
    let op = match operator.as_ref() {
        "+" => BinaryOp::Add,
        "-" => BinaryOp::Subtract,
        "*" => BinaryOp::Multiply,
        "/" => BinaryOp::Divide,
        "^" => BinaryOp::Power,
        "%/%" => BinaryOp::IntegerDivide,
        "%%" => BinaryOp::Modulo,
        "==" => BinaryOp::Equal,
        "!=" => BinaryOp::NotEqual,
        "<" => BinaryOp::Less,
        "<=" => BinaryOp::LessEqual,
        ">" => BinaryOp::Greater,
        ">=" => BinaryOp::GreaterEqual,
        "&" => BinaryOp::And,
        "|" => BinaryOp::Or,
        other => {
            return Err(error_in(
                call.call,
                format!("`{other}` does not apply to expressions"),
            ));
        }
    };
    expr(Expr::Binary(op, Box::new(left), Box::new(right)))
}

/// `.expr_aggregate(expr, function, na_rm)`. An aggregation skips `NA`s, as Polars' does, unless
/// `na_rm = FALSE` asks for R's rule, under which one `NA` makes the result `NA`.
unsafe fn do_aggregate(call: &Call<'_>) -> R<Sexp> {
    let arg = expression(call.arg(0), call.call)?;
    let function = match values::require_str(call.arg(1), "function")?.as_ref() {
        "sum" => Aggregate::Sum,
        "mean" => Aggregate::Mean,
        "min" => Aggregate::Min,
        "max" => Aggregate::Max,
        "first" => Aggregate::First,
        "last" => Aggregate::Last,
        "median" => Aggregate::Median,
        "std" => Aggregate::Sd,
        "var" => Aggregate::Var,
        "count" => Aggregate::Count,
        "n_unique" => Aggregate::NUnique,
        other => {
            return Err(error_in(
                call.call,
                format!("unknown aggregation `{other}`"),
            ));
        }
    };
    let na_rm = values::require_flag(call.arg(2), "na_rm")?;
    wrap(Object::Expr(Named {
        alias: arg.alias,
        expr: Expr::Aggregate {
            function,
            arg: Box::new(arg.expr),
            na_rm,
        },
    }))
}

unsafe fn do_len(_: &Call<'_>) -> R<Sexp> {
    expr(Expr::Count)
}

unsafe fn do_alias(call: &Call<'_>) -> R<Sexp> {
    let named = expression(call.arg(0), call.call)?;
    let alias = values::require_str(call.arg(1), "name")?;
    wrap(Object::Expr(Named {
        alias: Some(alias.into()),
        expr: named.expr,
    }))
}

unsafe fn do_if_else(call: &Call<'_>) -> R<Sexp> {
    let test = operand(call.arg(0), call.call)?;
    let yes = operand(call.arg(1), call.call)?;
    let no = operand(call.arg(2), call.call)?;
    expr(Expr::IfElse(Box::new(test), Box::new(yes), Box::new(no)))
}

unsafe fn do_cor(call: &Call<'_>) -> R<Sexp> {
    let first = operand(call.arg(0), call.call)?;
    let second = operand(call.arg(1), call.call)?;
    expr(Expr::Correlation(Box::new(first), Box::new(second)))
}

unsafe fn do_head_expr(call: &Call<'_>) -> R<Sexp> {
    let named = expression(call.arg(0), call.arg(2))?;
    let count = row_count(call.arg(1), call.arg(2))?;
    wrap(Object::Expr(Named {
        alias: named.alias,
        expr: Expr::Head(Box::new(named.expr), count),
    }))
}

/// `.expr_over(list(expr, keys...), call)`: the expression evaluated within each group of the key
/// columns, one value per row, as data.table's `:=` with `by` gives.
unsafe fn do_over(call: &Call<'_>) -> R<Sexp> {
    let caller = call.arg(1);
    let (subject, columns) = verb_arguments(call.arg(0), "over", Vectors::None, caller)?;
    let named = expression(subject, caller)?;
    let mut keys = Vec::with_capacity(columns.len());
    for (_, column) in columns {
        let Expr::Column(name) = column else {
            keys.clear();
            break;
        };
        keys.push(name);
    }
    if keys.is_empty() {
        return Err(error_in(
            caller,
            "`over` needs the key columns by name, as `over(x, \"id\")`",
        ));
    }
    wrap(Object::Expr(Named {
        alias: named.alias,
        expr: Expr::Window(Box::new(named.expr), keys),
    }))
}

/// `.expr_call(function, arguments, options, call)`: a [`Function`] of expressions and vectors,
/// with its options as R values in the order the `Expr` function takes them.
unsafe fn do_call(call: &Call<'_>) -> R<Sexp> {
    let caller = call.arg(3);
    let name = values::require_str(call.arg(0), "function")?;
    let options = call.arg(2);
    let option = |index: usize| {
        if index < length(options) {
            options.element(index)
        } else {
            nil()
        }
    };
    let text = |index: usize, what: &str| -> R<PlSmallStr> {
        Ok(values::require_str(option(index), what)?.as_ref().into())
    };
    let function = match name.as_ref() {
        "is_in" => Function::IsIn,
        "str_contains" => Function::Contains {
            literal: values::require_flag(option(0), "literal")?,
        },
        "str_starts_with" => Function::StartsWith,
        "str_ends_with" => Function::EndsWith,
        "str_replace" | "str_replace_all" => Function::Replace {
            all: name == "str_replace_all",
            literal: values::require_flag(option(0), "literal")?,
        },
        "str_slice" => {
            let length = match is_nil(option(1)) {
                true => None,
                false => Some(u64::try_from(whole(option(1), "length", caller)?).map_err(
                    |_| error_in(caller, "`length` must be a count of characters, 0 or more"),
                )?),
            };
            Function::Slice {
                start: whole(option(0), "start", caller)?,
                length,
            }
        }
        "str_to_uppercase" => Function::Uppercase,
        "str_to_lowercase" => Function::Lowercase,
        "str_to_date" => Function::ToDate {
            format: text(0, "format")?,
        },
        "concat_str" => Function::ConcatStr {
            separator: text(0, "separator")?,
        },
        "dt_year" => Function::Year,
        "dt_month" => Function::Month,
        "dt_day" => Function::Day,
        "dt_offset_by" => Function::OffsetBy { by: text(0, "by")? },
        "dt_to_string" => Function::Format {
            format: text(0, "format")?,
        },
        "fill_null" => Function::FillNull,
        "coalesce" => Function::Coalesce,
        "cast" => {
            let target = text(0, "type")?;
            match ColumnType::from_cast_name(&target) {
                Some(kind) if kind != ColumnType::Datetime => Function::Cast(kind),
                _ => {
                    return Err(error_in(
                        caller,
                        format!(
                            "`type` is \"logical\", \"integer\", \"double\", \"character\", \"factor\" or \"Date\", not \"{target}\""
                        ),
                    ));
                }
            }
        }
        "shift" => Function::Shift(whole(option(0), "n", caller)?),
        "cum_sum" => Function::CumSum,
        other => {
            return Err(error_in(
                caller,
                format!("no expression function `{other}`"),
            ));
        }
    };
    let list = call.arg(1);
    // `x %in% NULL` is `FALSE` for every value of `x`, `NA` included; `is.na(x) & FALSE` keeps
    // `x`'s name for the result.
    if function == Function::IsIn && length(list) == 2 && is_nil(list.element(1)) {
        let x = operand(list.element(0), caller)?;
        return expr(Expr::Binary(
            BinaryOp::And,
            Box::new(Expr::Unary(UnaryOp::IsNa, Box::new(x))),
            Box::new(Expr::Logical(false)),
        ));
    }
    let mut arguments = Vec::with_capacity(length(list));
    for index in 0..length(list) {
        arguments.push(operand(list.element(index), caller)?);
    }
    expr(Expr::Call(function, arguments))
}

/// A whole number option, such as `shift`'s `n`.
unsafe fn whole(value: Sexp, what: &str, call: Sexp) -> R<i64> {
    let number = match value.kind() {
        SexpType::Integer | SexpType::Real if length(value) == 1 => {
            values::as_real_scalar_warning(value)?
        }
        _ => f64::NAN,
    };
    if !number.is_finite() || number.fract() != 0.0 || number.abs() > f64::from(i32::MAX) {
        return Err(error_in(call, format!("`{what}` must be a whole number")));
    }
    Ok(number as i64)
}

unsafe fn do_rank(call: &Call<'_>) -> R<Sexp> {
    let caller = call.arg(2);
    let named = expression(call.arg(0), caller)?;
    let descending = values::require_flag(call.arg(1), "descending")?;
    wrap(Object::Expr(Named {
        alias: named.alias,
        expr: Expr::Rank {
            arg: Box::new(named.expr),
            descending,
        },
    }))
}

unsafe fn do_format(call: &Call<'_>) -> R<Sexp> {
    let named = expression(call.arg(0), call.call)?;
    let text = match &named.alias {
        Some(alias) => lazy::aliased(&named.expr, alias),
        None => named.expr.to_string(),
    };
    scalar_string(&text)
}

/// The plan a verb extends, and whether the result stays lazy.
unsafe fn plan_of(value: Sexp, call: Sexp) -> R<(Plan, bool)> {
    if is_frame(value) {
        return Ok((Plan::new(Source::Table(table_of(value)?.fork())), false));
    }
    match object_of(value) {
        Some(Object::Lazy(plan)) => Ok((plan.clone(), true)),
        _ => Err(error_in(
            call,
            format!(
                "expected a DataFrame or a LazyFrame, got {}",
                describe(value)
            ),
        )),
    }
}

/// `.df_concat(items, how, call)`: the frames of `items` (or of the one list it holds) stacked
/// by column name (`"vertical"`, every frame with the same columns; `"diagonal"`, a missing
/// column filled with `NA`), or set side by side (`"horizontal"`), where a named vector is a
/// column too.
unsafe fn do_concat(call: &Call<'_>) -> R<Sexp> {
    let caller = call.arg(2);
    let mut items = call.arg(0);
    if length(items) == 1 && items.element(0).kind() == SexpType::List {
        items = items.element(0);
    }
    let how = values::require_str(call.arg(1), "how")?;
    let names = values::names(items);
    let argument_name = |index: usize| {
        if is_nil(names) {
            None
        } else {
            values::string_at(names, index).filter(|name| !name.is_empty())
        }
    };
    let bind = match how.as_ref() {
        "vertical" => ops::Bind::Name,
        "diagonal" => ops::Bind::Fill,
        "horizontal" => {
            let height = (0..length(items))
                .map(|index| items.element(index))
                .find(|item| is_frame(*item))
                .map(|item| table_of(item).map(|table| table.height()))
                .transpose()?;
            let Some(height) = height else {
                return Err(error_in(caller, "`concat` needs at least one DataFrame"));
            };
            let mut columns = Vec::new();
            for index in 0..length(items) {
                let item = items.element(index);
                if is_frame(item) {
                    let table = table_of(item)?;
                    if table.height() != height {
                        return Err(error_in(
                            caller,
                            format!(
                                "frame {} has {} rows, but the first frame has {height}",
                                index + 1,
                                table.height()
                            ),
                        ));
                    }
                    columns.extend(table.frame().columns().iter().cloned());
                    continue;
                }
                let Some(name) = argument_name(index) else {
                    return Err(error_in(
                        caller,
                        format!(
                            "`concat` sets frames and named vectors side by side, not {}",
                            describe(item)
                        ),
                    ));
                };
                columns.push(frame::to_column(&name, item, height, caller)?);
            }
            let duplicate = columns.iter().enumerate().find_map(|(index, column)| {
                columns[..index]
                    .iter()
                    .any(|earlier| earlier.name() == column.name())
                    .then(|| column.name().clone())
            });
            if let Some(duplicate) = duplicate {
                return Err(error_in(
                    caller,
                    format!(
                        "column `{duplicate}` appears in more than one of the frames and vectors set side by side; rename one first"
                    ),
                ));
            }
            let combined = table::polars::prelude::DataFrame::new(height, columns)
                .map_err(|failure| error_in(caller, failure.to_string()))?;
            return frame::wrap(
                table::Table::new(combined)
                    .map_err(|failure| table_error(caller, failure, None))?,
            );
        }
        other => {
            return Err(error_in(
                caller,
                format!("`how` is \"vertical\", \"diagonal\" or \"horizontal\", not \"{other}\""),
            ));
        }
    };
    let mut tables = Vec::with_capacity(length(items));
    for index in 0..length(items) {
        let item = items.element(index);
        if !is_frame(item) {
            return Err(error_in(
                caller,
                format!("`concat` stacks DataFrames, not {}", describe(item)),
            ));
        }
        tables.push(&*table_of(item)?);
    }
    if bind == ops::Bind::Name
        && let Some(first) = tables.first()
    {
        for (index, table) in tables.iter().enumerate().skip(1) {
            let (missing, frame) = match first
                .frame()
                .get_column_names()
                .into_iter()
                .find(|name| table.frame().column(name).is_err())
            {
                Some(name) => (name.clone(), index + 1),
                None => match table
                    .frame()
                    .get_column_names()
                    .into_iter()
                    .find(|name| first.frame().column(name).is_err())
                {
                    Some(name) => (name.clone(), 1),
                    None => continue,
                },
            };
            return Err(error_in(
                caller,
                format!(
                    "frame {frame} has no column `{missing}`; to fill it with `NA`, use `how = \"diagonal\"`"
                ),
            ));
        }
    }
    let stacked =
        ops::rbind_list(&tables, bind).map_err(|failure| table_error(caller, failure, None))?;
    frame::wrap(stacked)
}

/// `.df_unique(df, subset, keep, call)`.
unsafe fn do_unique(call: &Call<'_>) -> R<Sexp> {
    let caller = call.arg(3);
    let (plan, lazy) = plan_of(call.arg(0), caller)?;
    let mut subset = Vec::new();
    if !is_nil(call.arg(1)) {
        for name in values::strings(call.arg(1)) {
            let Some(name) = name else {
                return Err(error_in(caller, "`subset` names a column `NA`"));
            };
            subset.push(PlSmallStr::from(name));
        }
    }
    let keep = values::require_str(call.arg(2), "keep")?;
    let Some(keep) = ops::Keep::parse(&keep) else {
        return Err(error_in(
            caller,
            format!("`keep` is \"first\", \"last\" or \"none\", not \"{keep}\""),
        ));
    };
    finish(plan.with(Step::Unique { subset, keep }), lazy, caller)
}

/// `.df_duplicated(df, from_last, call)`: whether each row repeats an earlier row, or a later one
/// with `from_last`.
unsafe fn do_duplicated(call: &Call<'_>) -> R<Sexp> {
    let caller = call.arg(2);
    if !is_frame(call.arg(0)) {
        return Err(error_in(
            caller,
            format!("expected a DataFrame, got {}", describe(call.arg(0))),
        ));
    }
    let from_last = values::require_flag(call.arg(1), "fromLast")?;
    let duplicated = ops::duplicated_rows(table_of(call.arg(0))?, &[], from_last)
        .map_err(|failure| table_error(caller, failure, None))?;
    let vector = values::alloc(SexpType::Logical, duplicated.len())?;
    for (slot, flag) in values::ints_mut(vector).iter_mut().zip(duplicated) {
        *slot = i32::from(flag);
    }
    Ok(vector)
}

/// `.df_drop(list(df, ...), call)`: every column but the named ones, which must exist.
unsafe fn do_drop(call: &Call<'_>) -> R<Sexp> {
    let caller = call.arg(1);
    let list = call.arg(0);
    let (plan, lazy, schema) = verb_frame(list, "drop", caller)?;
    let mut dropped: Vec<PlSmallStr> = Vec::new();
    for index in 1..length(list) {
        let item = list.element(index);
        if item.kind() != SexpType::String {
            return Err(error_in(
                caller,
                format!("`drop` takes column names, not {}", describe(item)),
            ));
        }
        for name in values::strings(item) {
            let Some(name) = name else {
                return Err(error_in(caller, "`drop` names a column `NA`"));
            };
            let name = PlSmallStr::from(name);
            if schema.column(&name).is_none() {
                return Err(table_error(
                    caller,
                    TableError::NoColumn(name.to_string()),
                    Some(&schema),
                ));
            }
            dropped.push(name);
        }
    }
    let kept = schema
        .names()
        .into_iter()
        .filter(|name| !dropped.contains(name))
        .map(|name| (name.clone(), Expr::Column(name.clone())))
        .collect();
    finish(plan.with(Step::Select(kept)), lazy, caller)
}

/// `.df_rename(list(df, new = "old", ...), call)`: the columns in place, each `old` one named
/// `new`, as `select` reads `new = "old"`.
unsafe fn do_rename(call: &Call<'_>) -> R<Sexp> {
    let caller = call.arg(1);
    let list = call.arg(0);
    let (plan, lazy, schema) = verb_frame(list, "rename", caller)?;
    let names = values::names(list);
    let mut renames: Vec<(PlSmallStr, PlSmallStr)> = Vec::new();
    for index in 1..length(list) {
        let item = list.element(index);
        let new = match is_nil(names) {
            true => None,
            false => values::string_at(names, index).filter(|name| !name.is_empty()),
        };
        let old = match item.kind() {
            SexpType::String if length(item) == 1 => values::string_at(item, 0),
            _ => None,
        };
        let (Some(new), Some(old)) = (new, old) else {
            return Err(error_in(
                caller,
                "`rename` takes each new name with the column it renames, as `new = \"old\"`",
            ));
        };
        let old = PlSmallStr::from(old.as_ref());
        if schema.column(&old).is_none() {
            return Err(table_error(
                caller,
                TableError::NoColumn(old.to_string()),
                Some(&schema),
            ));
        }
        if renames.iter().any(|(earlier, _)| *earlier == old) {
            return Err(error_in(caller, format!("`rename` renames `{old}` twice")));
        }
        renames.push((old, new.as_ref().into()));
    }
    let mut columns: Vec<(PlSmallStr, Expr)> = Vec::with_capacity(schema.columns.len());
    for (name, _) in &schema.columns {
        let output = renames
            .iter()
            .find(|(old, _)| old == name)
            .map_or_else(|| name.clone(), |(_, new)| new.clone());
        if columns.iter().any(|(taken, _)| *taken == output) {
            return Err(error_in(
                caller,
                format!("`rename` gives two columns the name `{output}`"),
            ));
        }
        columns.push((output, Expr::Column(name.clone())));
    }
    finish(plan.with(Step::Select(columns)), lazy, caller)
}

/// `.df_with_row_index(df, name, offset, call)`: a first column numbering the rows from `offset`.
unsafe fn do_with_row_index(call: &Call<'_>) -> R<Sexp> {
    let caller = call.arg(3);
    let (plan, lazy) = plan_of(call.arg(0), caller)?;
    let name = PlSmallStr::from(values::require_str(call.arg(1), "name")?.as_ref());
    let offset = whole(call.arg(2), "offset", caller)?;
    let schema = plan
        .schema()
        .map_err(|failure| table_error(caller, failure, plan.valid_schema().as_ref()))?;
    if schema.column(&name).is_some() {
        return Err(error_in(
            caller,
            format!("the frame already has a column `{name}`; pass another `name`"),
        ));
    }
    let mut columns = Vec::with_capacity(schema.columns.len() + 1);
    columns.push((name, Expr::Call(Function::RowIndex(offset), Vec::new())));
    columns.extend(
        schema
            .columns
            .iter()
            .map(|(name, _)| (name.clone(), Expr::Column(name.clone()))),
    );
    finish(plan.with(Step::Select(columns)), lazy, caller)
}

/// `.df_unpivot(df, on, index, variable_name, value_name, call)`: wide to long, as data.table's
/// `melt` reshapes. `on` defaults to every column outside `index`. A LazyFrame is collected first,
/// and the result stays lazy.
unsafe fn do_unpivot(call: &Call<'_>) -> R<Sexp> {
    let caller = call.arg(5);
    let (table, lazy) = materialized(call.arg(0), caller)?;
    let schema = table.schema();
    let column_names = |value: Sexp, what: &str| -> R<Option<Vec<String>>> {
        if is_nil(value) {
            return Ok(None);
        }
        if value.kind() != SexpType::String {
            return Err(error_in(
                caller,
                format!("`{what}` names columns, as `{what} = c(\"a\", \"b\")`"),
            ));
        }
        let mut names = Vec::with_capacity(length(value));
        for name in values::strings(value) {
            let Some(name) = name else {
                return Err(error_in(caller, format!("`{what}` names a column `NA`")));
            };
            names.push(name);
        }
        Ok(Some(names))
    };
    let index = column_names(call.arg(2), "index")?.unwrap_or_default();
    let on = match column_names(call.arg(1), "on")? {
        Some(on) => on,
        None => schema
            .names()
            .into_iter()
            .filter(|name| !index.iter().any(|kept| kept == name.as_str()))
            .map(|name| name.to_string())
            .collect(),
    };
    let variable = values::require_str(call.arg(3), "variable_name")?;
    let value = values::require_str(call.arg(4), "value_name")?;
    let index: Vec<&str> = index.iter().map(String::as_str).collect();
    let on: Vec<&str> = on.iter().map(String::as_str).collect();
    let executed = ops::melt(&table, &index, &on, &variable, &value)
        .map_err(|failure| table_error(caller, failure, Some(&schema)))?;
    raise(&executed.warnings, caller)?;
    let Output::Table(result) = executed.output else {
        return Err(error_in(caller, "`unpivot` gave no table"));
    };
    match lazy {
        true => wrap(Object::Lazy(Plan::new(Source::Table(result)))),
        false => frame::wrap(result),
    }
}

/// The table a verb that needs the data works on: a DataFrame's own, or a LazyFrame's, collected.
unsafe fn materialized(value: Sexp, caller: Sexp) -> R<(table::Table, bool)> {
    match object_of(value) {
        Some(Object::Lazy(plan)) => {
            let schema = plan.valid_schema();
            let collected = lazy::collect(plan.clone())
                .map_err(|failure| table_error(caller, failure, schema.as_ref()))?;
            raise(&collected.warnings, caller)?;
            Ok((collected.table, true))
        }
        _ if is_frame(value) => Ok((table_of(value)?.fork(), false)),
        _ => Err(error_in(
            caller,
            format!(
                "expected a DataFrame or a LazyFrame, got {}",
                describe(value)
            ),
        )),
    }
}

/// The frame a column verb's `...` starts with, as its plan and its schema.
unsafe fn verb_frame(list: Sexp, verb: &str, caller: Sexp) -> R<(Plan, bool, Schema)> {
    let names = values::names(list);
    let named = !is_nil(names) && values::string_at(names, 0).is_some_and(|name| !name.is_empty());
    if length(list) == 0 || named {
        return Err(error_in(
            caller,
            format!("`{verb}` takes the frame as its first argument, without a name"),
        ));
    }
    let (plan, lazy) = plan_of(list.element(0), caller)?;
    let schema = plan
        .schema()
        .map_err(|failure| table_error(caller, failure, plan.valid_schema().as_ref()))?;
    Ok((plan, lazy, schema))
}

/// Wraps an extended plan: a `LazyFrame` stays lazy, and a `DataFrame`'s one-step plan runs now.
unsafe fn finish(plan: Plan, lazy: bool, call: Sexp) -> R<Sexp> {
    if lazy {
        return wrap(Object::Lazy(plan));
    }
    collect(plan, call)
}

unsafe fn collect(plan: Plan, call: Sexp) -> R<Sexp> {
    let schema = plan.valid_schema();
    let collected =
        lazy::collect(plan).map_err(|failure| table_error(call, failure, schema.as_ref()))?;
    raise(&collected.warnings, call)?;
    frame::wrap(collected.table)
}

unsafe fn raise(warnings: &[Warning], call: Sexp) -> R<()> {
    for warning in warnings {
        conditions::warning_call(call, warning.to_string())?;
    }
    Ok(())
}

/// An engine error as an R error of the user's call. A missing column also lists the columns
/// there are, which is usually the fastest way to spot a typo.
unsafe fn table_error(
    call: Sexp,
    failure: TableError,
    schema: Option<&Schema>,
) -> interp::unwind::Unwind {
    let message = match (&failure, schema) {
        (TableError::NoColumn(name), Some(schema)) => {
            let columns: Vec<String> = schema
                .names()
                .iter()
                .map(|name| format!("`{name}`"))
                .collect();
            format!(
                "there is no column `{name}`; the columns are {}",
                columns.join(", ")
            )
        }
        _ => failure.to_string(),
    };
    error_in(call, message)
}

/// A verb's `...`: the value it works on, which is the first argument, and the named expressions
/// after it. An expression keeps its alias, or takes the argument's name. Strings without an
/// argument name name columns, and a single string with one names a column and renames it, as
/// Polars reads a string. `vectors` says what any other R vector is.
unsafe fn verb_arguments(
    list: Sexp,
    verb: &str,
    vectors: Vectors,
    call: Sexp,
) -> R<(Sexp, Vec<(PlSmallStr, Expr)>)> {
    if length(list) == 0 {
        return Err(error_in(
            call,
            format!("`{verb}` needs the value it works on as its first argument"),
        ));
    }
    let names = values::names(list);
    if !is_nil(names)
        && let Some(name) = values::string_at(names, 0).filter(|name| !name.is_empty())
    {
        return Err(error_in(
            call,
            format!(
                "`{verb}` takes the value it works on as its first argument, without a name: `{name} =` names a column"
            ),
        ));
    }
    let mut columns = Vec::with_capacity(length(list));
    for index in 1..length(list) {
        let item = list.element(index);
        let argument_name = if is_nil(names) {
            None
        } else {
            values::string_at(names, index).filter(|name| !name.is_empty())
        };
        if let Some(Object::Expr(named)) = object_of(item) {
            let mut named = named.clone();
            if let Some(name) = argument_name {
                named.alias = Some(name.as_ref().into());
            }
            columns.push(named.output());
            continue;
        }
        if item.kind() == SexpType::String
            && (argument_name.is_none() || length(item) == 1 || vectors != Vectors::Columns)
        {
            match argument_name.filter(|_| length(item) == 1) {
                Some(name) => match values::string_at(item, 0) {
                    Some(column) => {
                        columns.push((name.as_ref().into(), Expr::Column(column.as_ref().into())))
                    }
                    None => {
                        return Err(error_in(
                            call,
                            format!("`{name}` names no column, because it is `NA`"),
                        ));
                    }
                },
                None => {
                    for name in values::strings(item).into_iter().flatten() {
                        columns.push((name.as_str().into(), Expr::Column(name.into())));
                    }
                }
            }
            continue;
        }
        match vectors {
            Vectors::Columns => {}
            Vectors::Masks if argument_name.is_none() && item.kind() == SexpType::Logical => {
                columns.push((PlSmallStr::EMPTY, operand(item, call)?));
                continue;
            }
            Vectors::Masks => {
                return Err(error_in(
                    call,
                    format!(
                        "`{verb}` takes expressions such as `Expr.col(\"x\") > 0` and logical vectors, not {}",
                        describe(item)
                    ),
                ));
            }
            Vectors::None => {
                return Err(error_in(
                    call,
                    format!(
                        "`{verb}` takes column names and expressions such as `Expr.col(\"x\")`, not {}",
                        describe(item)
                    ),
                ));
            }
        }
        match (argument_name, vector_literal(item)?) {
            (Some(name), Some(literal)) => columns.push((name.as_ref().into(), literal)),
            (None, Some(_)) => {
                return Err(error_in(
                    call,
                    format!(
                        "`{verb}` needs a name for a vector, such as `{verb}(x = ...)`, to make it a column"
                    ),
                ));
            }
            (_, None) => {
                return Err(error_in(
                    call,
                    format!(
                        "`{verb}` takes column names, expressions such as `Expr.col(\"x\")` and named vectors, not {}",
                        describe(item)
                    ),
                ));
            }
        }
    }
    Ok((list.element(0), columns))
}

/// What a verb's `...` takes an R vector that names no column as.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Vectors {
    /// A named vector is a column of its values, for `select` and `with_columns`.
    Columns,
    /// An unnamed logical vector is a row mask, for `filter`.
    Masks,
    /// Nothing, so a misspelt option such as `sort`'s `desc = TRUE` cannot pass as a constant
    /// column.
    None,
}

unsafe fn do_select(call: &Call<'_>) -> R<Sexp> {
    let caller = call.arg(1);
    let (subject, columns) = verb_arguments(call.arg(0), "select", Vectors::Columns, caller)?;
    let (plan, lazy) = plan_of(subject, caller)?;
    finish(plan.with(Step::Select(columns)), lazy, caller)
}

unsafe fn do_with_columns(call: &Call<'_>) -> R<Sexp> {
    let caller = call.arg(1);
    let (subject, columns) = verb_arguments(call.arg(0), "with_columns", Vectors::Columns, caller)?;
    let (plan, lazy) = plan_of(subject, caller)?;
    finish(plan.with(Step::WithColumns(columns)), lazy, caller)
}

/// Several predicates must all hold, as Polars' `filter(a, b)` is `filter(a & b)`. A predicate
/// is an expression or a logical vector with one value per row.
unsafe fn do_filter(call: &Call<'_>) -> R<Sexp> {
    let caller = call.arg(1);
    let (subject, predicates) = verb_arguments(call.arg(0), "filter", Vectors::Masks, caller)?;
    let (plan, lazy) = plan_of(subject, caller)?;
    let predicate = predicates
        .into_iter()
        .map(|(_, next)| next)
        .reduce(|earlier, next| Expr::Binary(BinaryOp::And, Box::new(earlier), Box::new(next)));
    let Some(predicate) = predicate else {
        return Err(error_in(
            caller,
            "`filter` needs a predicate such as `Expr.col(\"x\") > 0`",
        ));
    };
    finish(plan.with(Step::Filter(predicate)), lazy, caller)
}

unsafe fn do_group_by(call: &Call<'_>) -> R<Sexp> {
    let caller = call.arg(1);
    let (subject, keys) = verb_arguments(call.arg(0), "group_by", Vectors::None, caller)?;
    let (plan, lazy) = plan_of(subject, caller)?;
    if keys.is_empty() {
        return Err(error_in(caller, "`group_by` needs at least one key column"));
    }
    wrap(Object::Group { plan, keys, lazy })
}

unsafe fn do_agg(call: &Call<'_>) -> R<Sexp> {
    let caller = call.arg(1);
    let (group, aggregates) = verb_arguments(call.arg(0), "agg", Vectors::None, caller)?;
    let Some(Object::Group { plan, keys, lazy }) = object_of(group) else {
        return Err(error_in(
            caller,
            format!(
                "`agg` needs the result of `group_by`, got {}",
                describe(group)
            ),
        ));
    };
    let step = Step::Aggregate {
        keys: keys.clone(),
        aggregates,
    };
    finish(plan.clone().with(step), *lazy, caller)
}

unsafe fn do_sort(call: &Call<'_>) -> R<Sexp> {
    let caller = call.arg(2);
    let (subject, columns) = verb_arguments(call.arg(0), "sort", Vectors::None, caller)?;
    let (plan, lazy) = plan_of(subject, caller)?;
    if columns.is_empty() {
        return Err(error_in(
            caller,
            "`sort` needs at least one column to sort by",
        ));
    }
    let descending = values::ints(interp::coerce::coerce_vector(
        call.arg(1),
        SexpType::Logical,
    )?)
    .to_vec();
    if descending.is_empty() || (descending.len() != 1 && descending.len() != columns.len()) {
        return Err(error_in(
            caller,
            "`descending` takes one flag, or one per sort column",
        ));
    }
    let keys = columns
        .into_iter()
        .enumerate()
        .map(|(index, (_, expr))| SortKey {
            expr,
            descending: descending[index % descending.len()] == 1,
        })
        .collect();
    finish(plan.with(Step::Sort(keys)), lazy, caller)
}

unsafe fn row_count(value: Sexp, call: Sexp) -> R<usize> {
    let count = values::as_integer_scalar(value);
    usize::try_from(count).map_err(|_| error_in(call, "`n` must be a count of rows, 0 or more"))
}

unsafe fn do_head(call: &Call<'_>) -> R<Sexp> {
    let caller = call.arg(2);
    let (plan, lazy) = plan_of(call.arg(0), caller)?;
    let count = row_count(call.arg(1), caller)?;
    finish(plan.with(Step::Head(count)), lazy, caller)
}

unsafe fn do_tail(call: &Call<'_>) -> R<Sexp> {
    let caller = call.arg(2);
    let (plan, lazy) = plan_of(call.arg(0), caller)?;
    let count = row_count(call.arg(1), caller)?;
    finish(plan.with(Step::Tail(count)), lazy, caller)
}

/// `.df_join(df, other, left_on, right_on, how, suffix, strategy, call)`, where `strategy` is
/// `NULL` except for an as-of join.
unsafe fn do_join(call: &Call<'_>) -> R<Sexp> {
    let caller = call.arg(7);
    let (plan, lazy) = plan_of(call.arg(0), caller)?;
    let (other, _) = plan_of(call.arg(1), caller)?;
    let cross = call.arg(4).kind() == SexpType::String
        && values::require_str(call.arg(4), "how")? == "cross";
    let keys = |value: Sexp, side: &str| -> R<Vec<PlSmallStr>> {
        if cross && is_nil(value) {
            return Ok(Vec::new());
        }
        if value.kind() != SexpType::String || length(value) == 0 {
            return Err(error_in(
                caller,
                format!("`join` needs the key columns, as `on = \"id\"` or `{side} = \"id\"`"),
            ));
        }
        Ok(values::strings(value)
            .into_iter()
            .flatten()
            .map(PlSmallStr::from)
            .collect())
    };
    let left_on = keys(call.arg(2), "left_on")?;
    let right_on = keys(call.arg(3), "right_on")?;
    let how = values::require_str(call.arg(4), "how")?;
    let how = if how == "asof" {
        let strategy = values::require_str(call.arg(6), "strategy")?;
        How::Asof(Strategy::parse(&strategy).ok_or_else(|| {
            error_in(
                caller,
                format!("`strategy` is one of \"backward\", \"forward\" and \"nearest\", not \"{strategy}\""),
            )
        })?)
    } else {
        How::parse(&how).ok_or_else(|| {
            error_in(caller, format!("`how` is one of \"inner\", \"left\", \"full\", \"semi\", \"anti\" and \"cross\", not \"{how}\""))
        })?
    };
    let suffix = values::require_str(call.arg(5), "suffix")?;
    let step = Step::Join {
        other: Box::new(other),
        left_on,
        right_on,
        how,
        suffix: suffix.into(),
    };
    finish(plan.with(step), lazy, caller)
}

unsafe fn do_lazy(call: &Call<'_>) -> R<Sexp> {
    let table = table_of(call.arg(0))?.fork();
    wrap(Object::Lazy(Plan::new(Source::Table(table))))
}

unsafe fn lazy_plan(value: Sexp, call: Sexp) -> R<Plan> {
    match object_of(value) {
        Some(Object::Lazy(plan)) => Ok(plan.clone()),
        _ => Err(error_in(
            call,
            format!("expected a LazyFrame, got {}", describe(value)),
        )),
    }
}

unsafe fn do_collect(call: &Call<'_>) -> R<Sexp> {
    let caller = call.arg(1);
    collect(lazy_plan(call.arg(0), caller)?, caller)
}

unsafe fn do_explain(call: &Call<'_>) -> R<Sexp> {
    let caller = call.arg(1);
    let plan = lazy_plan(call.arg(0), caller)?;
    let schema = plan.valid_schema();
    let text =
        lazy::explain(plan).map_err(|failure| table_error(caller, failure, schema.as_ref()))?;
    scalar_string(&text)
}

/// `.df_pivot(df, on, index, values, agg, call)`: long to wide, one row per `index` combination
/// and one column per distinct value of `on`. It runs eagerly, because the output columns depend
/// on the data.
unsafe fn do_pivot(call: &Call<'_>) -> R<Sexp> {
    let caller = call.arg(5);
    let (table, _) = materialized(call.arg(0), caller)?;
    let on = values::require_str(call.arg(1), "on")?;
    let index = call.arg(2);
    if index.kind() != SexpType::String || length(index) == 0 {
        return Err(error_in(
            caller,
            "`pivot` needs the row key columns by name, as `index = \"id\"`",
        ));
    }
    let index: Vec<String> = values::strings(index).into_iter().flatten().collect();
    let index: Vec<&str> = index.iter().map(String::as_str).collect();
    let value = values::require_str(call.arg(3), "values")?;
    let function = if is_nil(call.arg(4)) {
        None
    } else {
        let name = values::require_str(call.arg(4), "agg")?;
        Some(match name.as_ref() {
            "sum" => Aggregate::Sum,
            "mean" => Aggregate::Mean,
            "min" => Aggregate::Min,
            "max" => Aggregate::Max,
            "first" => Aggregate::First,
            "last" => Aggregate::Last,
            "count" => Aggregate::Count,
            "len" => Aggregate::Length,
            other => {
                return Err(error_in(
                    caller,
                    format!(
                        "`agg` is one of \"sum\", \"mean\", \"min\", \"max\", \"first\", \"last\", \"count\" and \"len\", not \"{other}\""
                    ),
                ));
            }
        })
    };
    let schema = table.schema();
    let executed = ops::dcast(&table, &index, &on, &value, function)
        .map_err(|failure| table_error(caller, failure, Some(&schema)))?;
    raise(&executed.warnings, caller)?;
    match executed.output {
        Output::Table(table) => frame::wrap(table),
        _ => Err(error_in(caller, "`pivot` gave no table")),
    }
}

unsafe fn do_scan(call: &Call<'_>) -> R<Sexp> {
    let path = values::require_str(call.arg(0), "path")?.into_owned();
    let plan = match values::require_str(call.arg(1), "format")?.as_ref() {
        "parquet" => Plan::scan_parquet(path),
        _ => Plan::scan_csv(path),
    };
    wrap(Object::Lazy(plan))
}
