//! The Polars-style `DataFrame` API: column expressions built with `col("x")` and the verbs
//! `DataFrame.select`, `DataFrame.filter`, `DataFrame.group_by` with `GroupBy.agg`, and the rest,
//! called by qualified name and chained with `|>`.
//!
//! An expression, a `LazyFrame` and a `GroupBy` are external pointers that own a Rust value
//! ([`Object`]) and carry an R class, so R code passes them around like any other value. Arithmetic
//! and comparisons on an expression reach `Ops.Expr`, which builds the larger expression instead
//! of computing anything. A verb on a `DataFrame` runs at once as a one-step [`Plan`]; the same verb
//! on a `LazyFrame` appends the step, and `LazyFrame.collect` optimizes and runs the whole plan.

use {
    crate::{
        builtins::{BuiltinDef, Call, builtin},
        conditions,
        frame::{self, is_frame, literal, table_of},
        unwind::{R, error_in},
        values::{self, install, is_nil, length, nil, scalar_string, set_attr, symbols},
    },
    heap::object::{Sexp, SexpType},
    std::{cell::Cell, ffi::c_void},
    table::{
        Error as TableError, Schema,
        lazy::{self, How, Plan, Source, Step},
        polars::prelude::PlSmallStr,
        query::{Aggregate, BinaryOp, Expr, SortKey, UnaryOp, Warning},
    },
};

pub const BUILTINS: &[BuiltinDef] = &[
    builtin(".expr_column", 1, do_column),
    builtin(".expr_literal", 1, do_literal),
    builtin(".expr_operator", 4, do_operator),
    builtin(".expr_aggregate", 2, do_aggregate),
    builtin(".expr_len", 0, do_len),
    builtin(".expr_alias", 2, do_alias),
    builtin(".expr_if_else", 3, do_if_else),
    builtin(".expr_cor", 2, do_cor),
    builtin(".expr_format", 1, do_format),
    builtin(".df_select", 3, do_select),
    builtin(".df_with_columns", 3, do_with_columns),
    builtin(".df_filter", 3, do_filter),
    builtin(".df_group_by", 3, do_group_by),
    builtin(".df_agg", 3, do_agg),
    builtin(".df_sort", 4, do_sort),
    builtin(".df_head", 3, do_head),
    builtin(".df_tail", 3, do_tail),
    builtin(".df_join", 7, do_join),
    builtin(".df_lazy", 1, do_lazy),
    builtin(".df_collect", 2, do_collect),
    builtin(".df_explain", 2, do_explain),
    builtin(".df_scan", 2, do_scan),
];

/// The API, bound in the `ry` namespace and attached with the rest of `package:ry`. Each verb
/// passes `sys.call()` so an error names the call the user wrote, not the internal one.
pub const PACKAGE: &str = r#"
col <- function(x, as.factor = FALSE) {
  if (is.character(x)) .expr_column(x) else base::col(x, as.factor)
}
lit <- function(value) .expr_literal(value)
Expr.alias <- function(expr, name) .expr_alias(expr, name)
Expr.sum <- function(expr) .expr_aggregate(expr, "sum")
Expr.mean <- function(expr) .expr_aggregate(expr, "mean")
Expr.min <- function(expr) .expr_aggregate(expr, "min")
Expr.max <- function(expr) .expr_aggregate(expr, "max")
Expr.first <- function(expr) .expr_aggregate(expr, "first")
Expr.last <- function(expr) .expr_aggregate(expr, "last")
Expr.median <- function(expr) .expr_aggregate(expr, "median")
Expr.std <- function(expr) .expr_aggregate(expr, "std")
Expr.var <- function(expr) .expr_aggregate(expr, "var")
Expr.count <- function(expr) .expr_aggregate(expr, "count")
Expr.is_null <- function(expr) .expr_operator("is_null", expr, NULL, TRUE)
Expr.len <- function() .expr_len()
Expr.if_else <- function(test, yes, no) .expr_if_else(test, yes, no)
Expr.cor <- function(first, second) .expr_cor(first, second)
DataFrame.height <- function(df) dim(df)[[1L]]
DataFrame.width <- function(df) dim(df)[[2L]]
DataFrame.columns <- function(df) names(df)
DataFrame.select <- function(df, ...) .df_select(df, list(...), sys.call())
DataFrame.with_columns <- function(df, ...) .df_with_columns(df, list(...), sys.call())
DataFrame.filter <- function(df, ...) .df_filter(df, list(...), sys.call())
DataFrame.group_by <- function(df, ...) .df_group_by(df, list(...), sys.call())
DataFrame.sort <- function(df, ..., descending = FALSE) .df_sort(df, list(...), descending, sys.call())
DataFrame.head <- function(df, n = 5L) .df_head(df, n, sys.call())
DataFrame.tail <- function(df, n = 5L) .df_tail(df, n, sys.call())
DataFrame.join <- function(df, other, on = NULL, how = "inner", left_on = on, right_on = on, suffix = "_right") {
  .df_join(df, other, left_on, right_on, how, suffix, sys.call())
}
DataFrame.lazy <- function(df) .df_lazy(df)
GroupBy.agg <- function(group, ...) .df_agg(group, list(...), sys.call())
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
    format.Expr = function(x, ...) .expr_format(x),
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
    let heap = crate::heap();
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
        None => format!("a {} vector", value.kind().name()),
    }
}

// Expressions.

/// An operand: an expression, or a single R value as a literal.
unsafe fn operand(value: Sexp, call: Sexp) -> R<Expr> {
    if let Some(Object::Expr(named)) = object_of(value) {
        return Ok(named.expr.clone());
    }
    literal(value).ok_or_else(|| {
        error_in(
            call,
            format!(
                "an expression combines with expressions and single values, not {}",
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
                "expected an expression such as `col(\"x\")`, got {}",
                describe(value)
            ),
        )),
    }
}

unsafe fn expr(expr: Expr) -> R<Sexp> {
    wrap(Object::Expr(Named { alias: None, expr }))
}

unsafe fn do_column(call: &Call<'_>) -> R<Sexp> {
    let name = values::require_string(call.arg(0), "name")?;
    expr(Expr::Column(name.into()))
}

unsafe fn do_literal(call: &Call<'_>) -> R<Sexp> {
    expr(operand(call.arg(0), call.call)?)
}

unsafe fn do_operator(call: &Call<'_>) -> R<Sexp> {
    let operator = values::require_string(call.arg(0), "operator")?;
    let left = operand(call.arg(1), call.call)?;
    if values::require_flag(call.arg(3), "unary")? {
        let op = match operator.as_str() {
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
    let op = match operator.as_str() {
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

/// An aggregation skips `NA`s, as Polars' does.
unsafe fn do_aggregate(call: &Call<'_>) -> R<Sexp> {
    let arg = expression(call.arg(0), call.call)?;
    let function = match values::require_string(call.arg(1), "function")?.as_str() {
        "sum" => Aggregate::Sum,
        "mean" => Aggregate::Mean,
        "min" => Aggregate::Min,
        "max" => Aggregate::Max,
        "first" => Aggregate::First,
        "last" => Aggregate::Last,
        "median" => Aggregate::Median,
        "std" => Aggregate::Sd,
        "var" => Aggregate::Var,
        "count" => Aggregate::Length,
        other => {
            return Err(error_in(
                call.call,
                format!("unknown aggregation `{other}`"),
            ));
        }
    };
    let na_rm = !matches!(
        function,
        Aggregate::First | Aggregate::Last | Aggregate::Length
    );
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
    let alias = values::require_string(call.arg(1), "name")?;
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

unsafe fn do_format(call: &Call<'_>) -> R<Sexp> {
    let named = expression(call.arg(0), call.call)?;
    let text = match &named.alias {
        Some(alias) => lazy::aliased(&named.expr, alias),
        None => named.expr.to_string(),
    };
    scalar_string(&text)
}

// Verbs.

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
) -> crate::unwind::Unwind {
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

/// The named expressions of a verb's `...`: an expression keeps its alias, or takes the
/// argument's name, and a string names a column.
unsafe fn named_expressions(list: Sexp, verb: &str, call: Sexp) -> R<Vec<(PlSmallStr, Expr)>> {
    let names = values::names(list);
    let mut columns = Vec::with_capacity(length(list));
    for index in 0..length(list) {
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
        if item.kind() == SexpType::String {
            for name in values::strings(item).into_iter().flatten() {
                columns.push((name.as_str().into(), Expr::Column(name.into())));
            }
            continue;
        }
        return Err(error_in(
            call,
            format!(
                "`{verb}` takes column names and expressions such as `col(\"x\")`, not {}",
                describe(item)
            ),
        ));
    }
    Ok(columns)
}

unsafe fn do_select(call: &Call<'_>) -> R<Sexp> {
    let caller = call.arg(2);
    let (plan, lazy) = plan_of(call.arg(0), caller)?;
    let columns = named_expressions(call.arg(1), "select", caller)?;
    finish(plan.with(Step::Select(columns)), lazy, caller)
}

unsafe fn do_with_columns(call: &Call<'_>) -> R<Sexp> {
    let caller = call.arg(2);
    let (plan, lazy) = plan_of(call.arg(0), caller)?;
    let columns = named_expressions(call.arg(1), "with_columns", caller)?;
    finish(plan.with(Step::WithColumns(columns)), lazy, caller)
}

/// Several predicates must all hold, as Polars' `filter(a, b)` is `filter(a & b)`.
unsafe fn do_filter(call: &Call<'_>) -> R<Sexp> {
    let caller = call.arg(2);
    let (plan, lazy) = plan_of(call.arg(0), caller)?;
    let list = call.arg(1);
    let mut predicate: Option<Expr> = None;
    for index in 0..length(list) {
        let next = expression(list.element(index), caller)?.expr;
        predicate = Some(match predicate {
            Some(earlier) => Expr::Binary(BinaryOp::And, Box::new(earlier), Box::new(next)),
            None => next,
        });
    }
    let Some(predicate) = predicate else {
        return Err(error_in(
            caller,
            "`filter` needs a predicate such as `col(\"x\") > 0`",
        ));
    };
    finish(plan.with(Step::Filter(predicate)), lazy, caller)
}

unsafe fn do_group_by(call: &Call<'_>) -> R<Sexp> {
    let caller = call.arg(2);
    let (plan, lazy) = plan_of(call.arg(0), caller)?;
    let keys = named_expressions(call.arg(1), "group_by", caller)?;
    if keys.is_empty() {
        return Err(error_in(caller, "`group_by` needs at least one key column"));
    }
    wrap(Object::Group { plan, keys, lazy })
}

unsafe fn do_agg(call: &Call<'_>) -> R<Sexp> {
    let caller = call.arg(2);
    let Some(Object::Group { plan, keys, lazy }) = object_of(call.arg(0)) else {
        return Err(error_in(
            caller,
            format!(
                "`agg` needs the result of `group_by`, got {}",
                describe(call.arg(0))
            ),
        ));
    };
    let aggregates = named_expressions(call.arg(1), "agg", caller)?;
    let step = Step::Aggregate {
        keys: keys.clone(),
        aggregates,
    };
    finish(plan.clone().with(step), *lazy, caller)
}

unsafe fn do_sort(call: &Call<'_>) -> R<Sexp> {
    let caller = call.arg(3);
    let (plan, lazy) = plan_of(call.arg(0), caller)?;
    let columns = named_expressions(call.arg(1), "sort", caller)?;
    if columns.is_empty() {
        return Err(error_in(
            caller,
            "`sort` needs at least one column to sort by",
        ));
    }
    let descending = values::ints(crate::coerce::coerce_vector(
        call.arg(2),
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

/// `.df_join(df, other, left_on, right_on, how, suffix, call)`.
unsafe fn do_join(call: &Call<'_>) -> R<Sexp> {
    let caller = call.arg(6);
    let (plan, lazy) = plan_of(call.arg(0), caller)?;
    let (other, _) = plan_of(call.arg(1), caller)?;
    let keys = |value: Sexp, side: &str| -> R<Vec<PlSmallStr>> {
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
    let how = values::require_string(call.arg(4), "how")?;
    let how = How::parse(&how).ok_or_else(|| {
        error_in(caller, format!("`how` is one of \"inner\", \"left\", \"full\", \"semi\" and \"anti\", not \"{how}\""))
    })?;
    let suffix = values::require_string(call.arg(5), "suffix")?;
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

unsafe fn do_scan(call: &Call<'_>) -> R<Sexp> {
    let path = values::require_string(call.arg(0), "path")?;
    let plan = match values::require_string(call.arg(1), "format")?.as_str() {
        "parquet" => Plan::scan_parquet(path),
        _ => Plan::scan_csv(path),
    };
    wrap(Object::Lazy(plan))
}
