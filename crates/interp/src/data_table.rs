//! data.table queries on ry's engine.
//!
//! `DT[i, j, by]` on a data.table runs on ry's engine when the engine can run the whole query, and
//! otherwise goes to data.table's own `[.data.table` with its arguments untouched. The dispatch
//! of `[` calls [`query`] before it builds the promises for an S3 method, so a query the engine
//! takes never reaches data.table, and one it declines reaches data.table exactly as GNU R would
//! send it.
//!
//! Declining must be invisible, which shapes the rules here:
//!
//! - The query lowers in strict mode ([`Lowering::strict`]), which evaluates only expressions
//!   without side effects, so data.table running the query afterwards evaluates nothing twice.
//! - Any error while lowering or running declines, and data.table raises its own error.
//! - A query whose engine run raised a warning declines, because data.table words its warnings
//!   its own way.
//! - Only a data.table the engine represents exactly qualifies: classed `data.table` and
//!   `data.frame` alone, with no key or index (whose upkeep is data.table's), and columns that are
//!   attribute-free logical, integer, double or character vectors, or plain factors.
//! - Only code data.table itself treats as data.table-aware qualifies (data.table's `cedta`):
//!   anything outside a namespace, and namespaces that import data.table or set
//!   `.datatable.aware`. data.table's own namespace does not, so its internals keep their
//!   semantics.
//!
//! A result is built as data.table builds its own: the columns as a list, then data.table's
//! `setattr` and `setalloccol`, so printing, `.internal.selfref` and over-allocation are
//! data.table's. `:=` writes through `data.table::set()`, which shares `:=`'s C code, so updates
//! happen by reference, an update through `i` writes into the existing column, and a modified key
//! column drops the key, all as data.table does them.
//!
//! Integer and double columns enter the engine without a copy: the engine reads the R vector's
//! memory, which is safe because every engine value built from it is dropped before [`query`]
//! returns.

use {
    crate::{
        eval,
        frame::{self, Lowering, Piece, Plan},
        interp, scope,
        unwind::{R, Unwind, error},
        values::{self, install, is_nil, length, nil, symbols},
    },
    heap::object::{Sexp, SexpType},
    rustc_hash::{FxHashMap, FxHashSet},
    std::{cell::Cell, collections::HashSet},
    table::{
        ColumnType, Schema, Table,
        convert::{self, RVector},
        polars::prelude::{
            ChunkTake, DataFrame, DataType, IdxCa, IdxSize, IntoColumn, IntoSeries,
            NewChunkedArray, PlSmallStr, Series, StringChunked,
        },
        query::{self, Output, Query, Rows, Select},
    },
};

/// R helpers in the `ry` namespace. They call into data.table, which may not be installed, so
/// they are only called once a data.table exists.
const HELPERS: &str = r#"
.datatable_aware <- function(env) {
  ns <- topenv(env)
  if (!isNamespace(ns)) return(TRUE)
  name <- getNamespaceName(ns)
  name != "data.table" &&
    ("data.table" %in% names(getNamespaceImports(ns)) || isTRUE(get0(".datatable.aware", ns, inherits = FALSE)))
}
.datatable_result <- function(columns, names, sorted, shape) {
  rows <- c(NA_integer_, -(if (length(columns)) length(columns[[1L]]) else 0L))
  class <- c("data.table", "data.frame")
  # `setalloccol()` replaces a placeholder `.internal.selfref` where it stands and appends one
  # otherwise, which puts every attribute where data.table's own code path puts it.
  attributes(columns) <- switch(shape,
    list(row.names = rows, class = class, .internal.selfref = FALSE, names = names),
    list(names = names, row.names = rows, class = class),
    c(list(row.names = rows, class = class, names = names), if (length(sorted)) list(sorted = sorted))
  )
  data.table::setalloccol(columns)
}
.datatable_room <- function(x, added) {
  data.table:::selfrefok(x) > 0L && data.table::truelength(x) >= length(x) + added
}
.datatable_set <- function(x, i, j, value) data.table::set(x, i, j, value)
# `:=` returns the table visibly and leaves its address in `.global$print`, which is how
# `print.data.table()` skips the autoprint at the prompt but still prints in nested frames.
.datatable_updated <- function(x) assign("print", data.table::address(x), envir = data.table:::.global)
"#;

thread_local! {
    static NAMESPACE: Cell<Option<Sexp>> = const { Cell::new(None) };
}

/// Defines the helpers in `namespace`, the `ry` namespace, which is never collected.
pub unsafe fn attach(namespace: Sexp) -> R<()> {
    crate::session::eval_text(HELPERS, namespace)?;
    NAMESPACE.with(|cell| cell.set(Some(namespace)));
    Ok(())
}

/// Runs `subject[...]`, the `[` call `call` evaluated in `env`, on ry's engine, or returns `None`
/// to leave it to data.table.
pub unsafe fn query(call: Sexp, subject: Sexp, env: Sexp) -> R<Option<Sexp>> {
    if NAMESPACE.with(Cell::get).is_none() || !enabled() {
        return Ok(None);
    }
    let value = if is_plain(subject) {
        match scope(|| attempt(call, subject, env)) {
            Err(Unwind::Error(_)) => None,
            result => result?,
        }
    } else {
        None
    };
    if option_is_true("ry.datatable.trace") {
        let engine = if value.is_some() { "ry" } else { "data.table" };
        crate::write_console(
            format!("[{engine}] {}\n", crate::deparse::deparse_one_line(call)).as_bytes(),
            true,
        );
    }
    Ok(value)
}

unsafe fn option_is_true(name: &str) -> bool {
    let option = crate::envir::get_option(name);
    option.kind() == SexpType::Logical
        && length(option) == 1
        && values::as_logical_scalar(option) == 1
}

/// Whether `options(ry.datatable = FALSE)` is unset. `options(ry.datatable.trace = TRUE)` reports
/// on standard error which engine answered each query.
unsafe fn enabled() -> bool {
    let option = crate::envir::get_option("ry.datatable");
    !(option.kind() == SexpType::Logical
        && length(option) == 1
        && values::as_logical_scalar(option) == 0)
}

/// Whether `subject` is a data.table classed `data.table` and `data.frame` alone, with no key and
/// no attribute data.table does not set itself. An index is allowed: data.table only uses one to
/// find the rows `x[col == value]` selects faster, and the engine never sees those filters.
unsafe fn is_plain(subject: Sexp) -> bool {
    if subject.kind() != SexpType::List {
        return false;
    }
    let class = values::get_attr(subject, symbols().class);
    if values::strings(class)
        .iter()
        .map(Option::as_deref)
        .ne([Some("data.table"), Some("data.frame")])
    {
        return false;
    }
    let mut attribute = subject.attrib();
    while attribute.kind() == SexpType::Pairlist {
        let name = values::symbol_name(attribute.tag());
        if !matches!(
            name.as_ref(),
            "names" | "row.names" | "class" | ".internal.selfref" | "index"
        ) {
            return false;
        }
        attribute = attribute.cdr();
    }
    true
}

unsafe fn attempt(call: Sexp, subject: Sexp, env: Sexp) -> R<Option<Sexp>> {
    if values::as_logical_scalar(helper(".datatable_aware", &[env])?) != 1 {
        return Ok(None);
    }
    let Some((columns, height)) = subject_columns(subject)? else {
        return Ok(None);
    };
    let schema = Schema::new(
        columns
            .iter()
            .map(|column| (column.name.clone(), column.kind)),
    );
    let parts = frame::parse_parts(call, env, true)?;
    if let Some(i) = parts.i.as_ref().and_then(Piece::expression)
        && auto_indexes(i, &schema)
    {
        return Ok(None);
    }
    let lowering = Lowering {
        schema: &schema,
        height,
        env,
        call,
        strict: true,
    };
    let grouped = parts.by.is_some();
    let assigning = parts
        .j
        .as_ref()
        .and_then(Piece::expression)
        .is_some_and(|j| is_call(j, ":="));
    if let Some(j) = parts.j.as_ref().and_then(Piece::expression)
        && !reductions_agree(j, grouped && !assigning, &schema)
    {
        return Ok(None);
    }
    let (parts, plan) = frame::plan(parts, &lowering)?;
    let Plan {
        rows: Some(rows),
        select: Some(select),
        by: Some(by),
        assigns,
    } = plan
    else {
        return Ok(None);
    };
    // Without `on =`, data.table joins on the key, and a keyed subject declines.
    if matches!(rows, Rows::Join(_)) && parts.on.is_none() {
        return Ok(None);
    }
    let query = Query {
        rows,
        select,
        by,
        keyed: parts.keyed,
    };
    // Only the columns the query reads enter the engine, because importing a character column
    // converts every string.
    let mut table = import(&columns, height, query::referenced_columns(&query))?;
    if assigns {
        return update(subject, table, query, call);
    }
    // `x[]` returns `x` itself, which only data.table can do.
    if parts.i.is_none() && parts.j.is_none() {
        return Ok(None);
    }
    let shape = if !query.by.is_empty() {
        Shape::Grouped
    } else if matches!(query.select, Select::All)
        || selects_columns(&query.select, parts.j.as_ref().and_then(Piece::expression))
    {
        Shape::Rows
    } else {
        Shape::Selected
    };
    let sorted = parts.keyed.then(|| {
        query
            .by
            .iter()
            .map(|(name, _)| name.to_string())
            .collect::<Vec<_>>()
    });
    let Some(output) = run(&mut table, query)? else {
        return Ok(None);
    };
    let value = match output {
        Output::Table(result) => result_table(&result, shape, sorted.as_deref())?,
        Output::Vector(series) => frame::column_value(&series.into_column())?,
        Output::Updated => return Ok(None),
    };
    interp().visible = true;
    Ok(Some(value))
}

/// Whether data.table answers `x[i]` through an index on the compared columns, which it creates
/// on `x` the first time: `i` is `col == value`, `col %in% values` or `col %chin% values`, or an
/// `&` of such comparisons. Leaving these to data.table keeps `x`'s `index` attribute exactly
/// where data.table puts it.
unsafe fn auto_indexes(i: Sexp, schema: &Schema) -> bool {
    if i.kind() != SexpType::Language || i.car().kind() != SexpType::Symbol {
        return false;
    }
    let arguments = values::pairlist_items(i.cdr());
    match (values::symbol_name(i.car()).as_ref(), arguments.as_slice()) {
        ("(", [(_, inner)]) => auto_indexes(*inner, schema),
        ("&", [(_, left), (_, right)]) => {
            auto_indexes(*left, schema) && auto_indexes(*right, schema)
        }
        ("==" | "%in%" | "%chin%", [(_, left), (_, right)]) => {
            let is_column = |side: Sexp| {
                side.kind() == SexpType::Symbol
                    && schema
                        .columns
                        .iter()
                        .any(|(name, _)| name.as_str() == values::symbol_name(side))
            };
            is_column(*left) || is_column(*right)
        }
        _ => false,
    }
}

/// Whether the engine reduces `j` exactly as data.table does. With `by` and without `:=`,
/// data.table runs a `j` of `.N` and `f(column)` terms through its GForce kernels, which add
/// doubles in row order as the engine does. Every other `j` calls base R's `sum` and `mean`,
/// which accumulate in `long double`, so a `j` that reduces outside GForce is left to data.table.
/// So are the reductions whose rounding the engine does not reproduce: `median`, `var`, `sd`,
/// `prod`, and `mean(x, na.rm = TRUE)` of doubles, which GForce also sums in `long double`.
unsafe fn reductions_agree(j: Sexp, gforce: bool, schema: &Schema) -> bool {
    let term_agrees = |function: &str, column: ColumnType, extra: bool| match function {
        "median" | "var" | "sd" | "prod" => false,
        "mean" => !(extra && column == ColumnType::Double),
        _ => true,
    };
    if gforce && let Some(terms) = gforce_terms(j, schema) {
        return terms.iter().all(|term| match term {
            Term::Count => true,
            Term::Reduce {
                function,
                column,
                extra,
            } => term_agrees(function, *column, *extra),
        });
    }
    !reduces(j)
}

/// One term of a `j` that data.table's GForce runs: `.N`, or `function(column, ...)`.
enum Term {
    Count,
    Reduce {
        function: String,
        column: ColumnType,
        extra: bool,
    },
}

/// The terms of `j` when it has data.table's GForce form: `.N`, a single `f(column)` term, a
/// `list()` or `.()` of them, or `lapply(.SD, f)`, which data.table expands to such a list.
/// `f` is one of data.table's `gfuns`, and a term may add only `na.rm`.
unsafe fn gforce_terms(j: Sexp, schema: &Schema) -> Option<Vec<Term>> {
    const FUNCTIONS: [&str; 14] = [
        "[", "[[", "head", "tail", "first", "last", "sum", "mean", "median", "min", "max", "var",
        "sd", "prod",
    ];
    let column_type = |symbol: Sexp| {
        (symbol.kind() == SexpType::Symbol)
            .then(|| values::symbol_name(symbol))
            .and_then(|name| {
                schema
                    .columns
                    .iter()
                    .find(|(column, _)| column.as_str() == name)
                    .map(|(_, kind)| *kind)
            })
    };
    let term = |q: Sexp| -> Option<Term> {
        if q.kind() == SexpType::Symbol && values::symbol_name(q) == ".N" {
            return Some(Term::Count);
        }
        if q.kind() != SexpType::Language || q.car().kind() != SexpType::Symbol {
            return None;
        }
        let function = values::symbol_name(q.car()).into_owned();
        let arguments = values::pairlist_items(q.cdr());
        let (_, first) = arguments.first()?;
        let column = column_type(*first)?;
        if !FUNCTIONS.contains(&function.as_str())
            || matches!(function.as_str(), "head" | "tail" | "[" | "[[")
        {
            return None;
        }
        match arguments.as_slice() {
            [_] => Some(Term::Reduce {
                function,
                column,
                extra: false,
            }),
            [_, (tag, _)] if !is_nil(*tag) && values::symbol_name(*tag).starts_with("na") => {
                Some(Term::Reduce {
                    function,
                    column,
                    extra: true,
                })
            }
            _ => None,
        }
    };
    if j.kind() != SexpType::Language || j.car().kind() != SexpType::Symbol {
        return term(j).map(|term| vec![term]);
    }
    match values::symbol_name(j.car()).as_ref() {
        "list" | "." => values::pairlist_items(j.cdr())
            .into_iter()
            .map(|(_, item)| term(item))
            .collect(),
        "lapply" => {
            let arguments = values::pairlist_items(j.cdr());
            let [(_, sd), (_, function), rest @ ..] = arguments.as_slice() else {
                return None;
            };
            if sd.kind() != SexpType::Symbol
                || values::symbol_name(*sd) != ".SD"
                || function.kind() != SexpType::Symbol
            {
                return None;
            }
            let function = values::symbol_name(*function).into_owned();
            if !FUNCTIONS.contains(&function.as_str()) || rest.len() > 1 {
                return None;
            }
            // The columns `.SD` covers are not known here, so every column counts as a double.
            Some(vec![Term::Reduce {
                function,
                column: ColumnType::Double,
                extra: !rest.is_empty(),
            }])
        }
        _ => term(j).map(|term| vec![term]),
    }
}

/// Whether `expression` calls one of the reductions whose rounding depends on how they sum.
unsafe fn reduces(expression: Sexp) -> bool {
    if expression.kind() != SexpType::Language {
        return false;
    }
    let head = expression.car();
    if head.kind() == SexpType::Symbol
        && matches!(
            values::symbol_name(head).as_ref(),
            "sum" | "mean" | "median" | "var" | "sd" | "prod" | "lapply"
        )
    {
        return true;
    }
    reduces(head)
        || values::pairlist_items(expression.cdr())
            .into_iter()
            .any(|(_, item)| reduces(item))
}

unsafe fn is_call(expression: Sexp, name: &str) -> bool {
    expression.kind() == SexpType::Language
        && expression.car().kind() == SexpType::Symbol
        && values::symbol_name(expression.car()) == name
}

/// Runs `query`, or returns `None` when it raised a warning.
unsafe fn run(table: &mut Table, query: Query) -> R<Option<Output>> {
    let executed = query::execute(table, query).map_err(|failure| error(failure.to_string()))?;
    Ok(executed.warnings.is_empty().then_some(executed.output))
}

/// `:=` on `subject`, computed on `table`, its copy in the engine, and written into `subject`
/// through `data.table::set()` once every value is known, so a decline leaves `subject` as it was.
unsafe fn update(subject: Sexp, mut table: Table, query: Query, call: Sexp) -> R<Option<Sexp>> {
    let Select::Assign(assignments) = &query.select else {
        return Ok(None);
    };
    let positions = match &query.rows {
        Rows::All => None,
        Rows::Filter(condition) => Some(frame::selected_positions(
            &mut table,
            Rows::Filter(condition.clone()),
            call,
        )?),
        _ => return Ok(None),
    };
    let deletes = assignments.iter().any(|(_, value)| value.is_none());
    if deletes && (positions.is_some() || !query.by.is_empty()) {
        return Ok(None);
    }
    let existing: Vec<String> = values::strings(values::names(subject))
        .into_iter()
        .flatten()
        .collect();
    let added = assignments
        .iter()
        .filter(|(name, _)| !existing.iter().any(|column| column == name.as_str()))
        .count();
    if added > 0 {
        let room = helper(
            ".datatable_room",
            &[subject, values::scalar_integer(added as i32)?],
        )?;
        if values::as_logical_scalar(room) != 1 {
            return Ok(None);
        }
    }
    let names: Vec<PlSmallStr> = assignments.iter().map(|(name, _)| name.clone()).collect();
    if run(&mut table, query)?.is_none() {
        return Ok(None);
    }
    let mut writes = Vec::with_capacity(names.len());
    for name in &names {
        let index = existing.iter().position(|column| column == name.as_str());
        let Some(column) = table.frame().column(name).ok() else {
            writes.push((name, None, nil()));
            continue;
        };
        let series = column.as_materialized_series();
        if let Some(index) = index
            && !same_type(subject.element(index), series.dtype())
        {
            return Ok(None);
        }
        let (rows, value) = match (&positions, index) {
            (Some(positions), Some(_)) => {
                let taken = series
                    .take_slice(positions)
                    .map_err(|failure| error(failure.to_string()))?;
                let rows: Vec<i32> = positions
                    .iter()
                    .map(|&position| position as i32 + 1)
                    .collect();
                (
                    Some(values::integer_vector(&rows)?),
                    frame::column_value(&taken.into_column())?,
                )
            }
            _ => (None, frame::column_value(column)?),
        };
        writes.push((name, rows, value));
    }
    for (name, rows, value) in writes {
        let j = values::scalar_string(name)?;
        helper(
            ".datatable_set",
            &[subject, rows.unwrap_or(nil()), j, value],
        )?;
    }
    helper(".datatable_updated", &[subject])?;
    interp().visible = true;
    Ok(Some(subject))
}

/// Whether the R column `column` and an engine column of `dtype` have the same R type, so that
/// writing the one into the other converts nothing.
unsafe fn same_type(column: Sexp, dtype: &DataType) -> bool {
    match (column.kind(), dtype) {
        (SexpType::Integer, DataType::Int32) => !values::inherits(column, "factor"),
        (SexpType::Real, DataType::Float64) => true,
        (SexpType::Logical, DataType::Boolean) => true,
        (SexpType::String, DataType::String) => true,
        _ => false,
    }
}

/// Which data.table code path builds a query's result, which decides the order of the result's
/// attributes, visible through `attributes()`.
#[derive(Clone, Copy)]
enum Shape {
    /// `x[i]`: `row.names`, `class`, `.internal.selfref`, `names`.
    Rows,
    /// `x[i, j]`: `names`, `row.names`, `class`, `.internal.selfref`.
    Selected,
    /// `x[i, j, by]`: `row.names`, `class`, `names`, `sorted`, `.internal.selfref`.
    Grouped,
}

/// Whether `j` picks columns as they are, the way `x[, c("a", "b")]`, `x[, -1]` or
/// `x[, cols, with = FALSE]` do, which data.table answers as it answers `x[i]`.
unsafe fn selects_columns(select: &Select, j: Option<Sexp>) -> bool {
    let Select::Columns(columns) = select else {
        return false;
    };
    let listed = j.is_some_and(|j| is_call(j, "list") || is_call(j, "."));
    !listed
        && columns.iter().all(
            |(name, expr)| matches!(expr, table::query::Expr::Column(column) if column == name),
        )
}

/// `result` as a new data.table, keyed by `sorted` when the query was a `keyby`.
unsafe fn result_table(result: &Table, shape: Shape, sorted: Option<&[String]>) -> R<Sexp> {
    let frame = result.frame();
    let columns = values::alloc(SexpType::List, frame.width())?;
    let mut names = Vec::with_capacity(frame.width());
    for (index, column) in frame.columns().iter().enumerate() {
        values::set_elt(columns, index, frame::column_value(column)?);
        names.push(column.name().to_string());
    }
    let names = values::string_vector(&names)?;
    let sorted = match sorted {
        Some(names) if !names.is_empty() => values::string_vector(names)?,
        _ => nil(),
    };
    helper(
        ".datatable_result",
        &[
            columns,
            names,
            sorted,
            values::scalar_integer(shape as i32 + 1)?,
        ],
    )
}

/// One column of the subject, typed without reading its data.
struct SubjectColumn {
    name: PlSmallStr,
    vector: Sexp,
    kind: ColumnType,
}

/// The columns of `subject` and its height, or `None` when a column has a type or an attribute
/// the engine does not represent exactly.
unsafe fn subject_columns(subject: Sexp) -> R<Option<(Vec<SubjectColumn>, usize)>> {
    let names = values::names(subject);
    if is_nil(names) {
        return Ok(None);
    }
    let width = length(subject);
    let mut seen = FxHashSet::default();
    let mut columns = Vec::with_capacity(width);
    let mut height = None;
    for index in 0..width {
        let Some(name) = values::string_at(names, index) else {
            return Ok(None);
        };
        if name.is_empty() || !seen.insert(name.to_string()) {
            return Ok(None);
        }
        let vector = subject.element(index);
        if *height.get_or_insert(length(vector)) != length(vector) {
            return Ok(None);
        }
        let plain = is_nil(vector.attrib());
        let kind = match vector.kind() {
            SexpType::Integer if plain => ColumnType::Integer,
            SexpType::Real if plain => ColumnType::Double,
            SexpType::Logical if plain => ColumnType::Logical,
            SexpType::String if plain => ColumnType::Character,
            SexpType::Integer if is_plain_factor(vector) => ColumnType::Factor,
            _ => return Ok(None),
        };
        columns.push(SubjectColumn {
            name: PlSmallStr::from(name.as_ref()),
            vector,
            kind,
        });
    }
    Ok(Some((columns, height.unwrap_or(0))))
}

/// The engine table of `columns`, keeping only those in `needed` when it is given.
unsafe fn import(
    columns: &[SubjectColumn],
    height: usize,
    needed: Option<HashSet<PlSmallStr>>,
) -> R<Table> {
    let mut imported = Vec::with_capacity(columns.len());
    let mut string_codes = Vec::new();
    for column in columns {
        if needed
            .as_ref()
            .is_some_and(|needed| !needed.contains(&column.name))
        {
            continue;
        }
        let (series, codes) = import_column(column)?;
        if let Some(codes) = codes {
            string_codes.push((column.name.clone(), codes));
        }
        imported.push(series.into_column());
    }
    let frame = DataFrame::new(height, imported).map_err(|failure| error(failure.to_string()))?;
    let mut table = Table::new(frame).map_err(|failure| error(failure.to_string()))?;
    for (name, codes) in string_codes {
        table
            .set_string_codes(&name, codes.codes, codes.first_rows)
            .map_err(|failure| error(failure.to_string()))?;
    }
    Ok(table)
}

/// A character column's rows numbered by distinct CHARSXP, as `Table::set_string_codes` takes
/// them.
struct ImportedCodes {
    codes: Vec<u32>,
    first_rows: Vec<u32>,
}

unsafe fn import_column(column: &SubjectColumn) -> R<(Series, Option<ImportedCodes>)> {
    let name = column.name.clone();
    let vector = values::materialize(column.vector)?;
    Ok(match column.kind {
        // The engine reads integer and double data where R keeps it, see the module docs.
        ColumnType::Integer => (convert::borrow_integer(name, values::ints(vector)), None),
        ColumnType::Double => (convert::borrow_double(name, values::reals(vector)), None),
        ColumnType::Logical => (
            to_series(name, RVector::Logical(values::ints(vector).to_vec()))?,
            None,
        ),
        ColumnType::Character => {
            // R interns its strings, so a column repeats a few CHARSXPs. Each distinct one
            // converts once, and the column gathers them, which copies a 16-byte view per row.
            let na = values::na_string();
            let mut positions: FxHashMap<Sexp, IdxSize> = FxHashMap::default();
            let mut distinct = Vec::new();
            let mut indices = Vec::with_capacity(length(vector));
            // The same numbering is the column's grouping codes: R interns its strings, so equal
            // strings are the same CHARSXP and the codes cost no string comparison.
            let mut codes = Vec::with_capacity(length(vector));
            let mut first_rows = Vec::new();
            let mut has_na = false;
            for index in 0..length(vector) {
                let charsxp = vector.element(index);
                if charsxp == na {
                    has_na = true;
                    indices.push(IdxSize::MAX);
                    codes.push(0);
                    continue;
                }
                let position = *positions.entry(charsxp).or_insert_with(|| {
                    distinct.push(values::char_str(charsxp));
                    first_rows.push(index as u32);
                    (distinct.len() - 1) as IdxSize
                });
                indices.push(position);
                codes.push(position as u32 + 1);
            }
            let indices = if has_na {
                IdxCa::from_iter_options(
                    PlSmallStr::EMPTY,
                    indices
                        .into_iter()
                        .map(|index| (index != IdxSize::MAX).then_some(index)),
                )
            } else {
                IdxCa::from_vec(PlSmallStr::EMPTY, indices)
            };
            let series = StringChunked::from_iter_values(name, distinct.iter().map(AsRef::as_ref))
                .take(&indices)
                .map_err(|failure| error(failure.to_string()))?
                .into_series();
            (series, Some(ImportedCodes { codes, first_rows }))
        }
        ColumnType::Factor => {
            let levels = values::get_attr(vector, symbols().levels);
            let levels: Vec<String> = values::strings(levels).into_iter().flatten().collect();
            if levels.len() != length(values::get_attr(vector, symbols().levels)) {
                return Err(error("a factor level is NA"));
            }
            let series = to_series(
                name,
                RVector::Factor {
                    codes: values::ints(vector).to_vec(),
                    levels,
                },
            )?;
            (series, None)
        }
    })
}

/// Whether `column` is a factor with only its levels and the class `"factor"`.
unsafe fn is_plain_factor(column: Sexp) -> bool {
    let class = values::get_attr(column, symbols().class);
    if values::strings(class)
        .iter()
        .map(Option::as_deref)
        .ne([Some("factor")])
    {
        return false;
    }
    let mut attribute = column.attrib();
    while attribute.kind() == SexpType::Pairlist {
        if attribute.tag() != symbols().levels && attribute.tag() != symbols().class {
            return false;
        }
        attribute = attribute.cdr();
    }
    true
}

unsafe fn to_series(name: PlSmallStr, vector: RVector) -> R<Series> {
    convert::to_series(name, vector).map_err(|failure| error(failure.to_string()))
}

/// Calls the helper `name` of the `ry` namespace.
unsafe fn helper(name: &str, arguments: &[Sexp]) -> R<Sexp> {
    let namespace = NAMESPACE
        .with(Cell::get)
        .ok_or_else(|| error("the ry namespace is not attached"))?;
    let function = eval::find_var_in_frame(install(name)?, namespace)?;
    eval::apply(
        nil(),
        function,
        &eval::value_args(arguments),
        values::base_env(),
    )
}
