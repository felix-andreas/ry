//! Frames, ry's built-in table, as runtime values. A frame is an external pointer that owns a
//! [`table::Table`], classed `"frame"`. The table is the only copy of the data: a column becomes
//! an R vector only when R code reads it.
//!
//! `x[i, j, by]` on a frame never reaches the `[` primitive. The evaluator and the bytecode hand
//! the call to [`query`] as soon as the subject is known to be a frame, before `i`, `j` and `by`
//! are evaluated, because those arguments name columns. [`query`] lowers the call into the engine's
//! query IR, so the whole query runs as one Polars plan. Only an argument the IR cannot express
//! runs as R code, evaluated in a data mask that binds the columns it mentions, once per group.
//!
//! Frames follow R's copy-on-modify rule rather than data.table's reference semantics. `:=`
//! updates the frame in place when nothing else refers to it, and otherwise updates a fork and
//! rebinds the variable the call named, so an alias taken earlier keeps the old columns. `setkey`
//! always forks and rebinds.

use crate::{
    builtins::{Arg, BuiltinDef, Call, builtin, special},
    conditions, eval,
    format::{Settings, display_width, format_real_common, format_real_with},
    heap, interp, scope,
    unwind::{R, Unwind, error, error_in},
    values::{
        self, char_str, install, is_nil, length, list_vector, missing_arg, na_string, nil,
        scalar_integer, scalar_string, set_attr, string_vector, symbol_name, symbols,
    },
    write_console,
};
use heap::object::{NA_INTEGER, Sexp, SexpType};
use rustc_hash::FxHashSet;
use std::{cell::Cell, ffi::c_void};
use table::{
    ColumnType, Table,
    convert::{self, RVector, is_na_real, string_to_factor},
    polars::prelude::{
        ChunkAgg, Column, CsvReadOptions, CsvWriter, DataFrame, DataType, IntoColumn, IntoLazy,
        ParquetReader, ParquetWriter, PlSmallStr, SerReader, SerWriter, col,
    },
    query::{
        self, Aggregate, BinaryOp, Expr, Join, Mult, Output, Query, Roll, Rows, Select, SortKey,
        UnaryOp,
    },
};

pub const BUILTINS: &[BuiltinDef] = &[
    special(".frame_new", -1, do_frame_new).keep_visibility(),
    builtin(".frame_from_list", 1, do_from_list),
    builtin(".frame_to_list", 1, do_to_list),
    builtin(".frame_read_csv", 2, do_read_csv),
    builtin(".frame_read_parquet", 1, do_read_parquet),
    builtin(".frame_write_csv", 2, do_write_csv).invisible(),
    builtin(".frame_write_parquet", 2, do_write_parquet).invisible(),
    builtin(".frame_type", 1, do_frame_type),
    builtin(".frame_key", 1, do_frame_key),
    builtin(".frame_copy", 1, do_frame_copy),
    builtin(".frame_setkey", 3, do_frame_setkey).invisible(),
    builtin("print.frame", -1, do_print).invisible(),
    builtin("$.frame", 2, do_dollar),
    builtin("[[.frame", -1, do_column),
    builtin("[.frame", -1, do_bracket_values),
    builtin("$<-.frame", 3, do_set_column),
    builtin("[[<-.frame", 3, do_set_column),
    builtin("names.frame", 1, do_names),
    builtin("dim.frame", 1, do_dim),
    builtin("dimnames.frame", 1, do_dimnames),
    builtin("length.frame", 1, do_length),
    builtin("as.list.frame", -1, do_to_list),
    builtin("head.frame", -1, do_head),
    builtin("tail.frame", -1, do_tail),
];

/// A registered namespace in the shape `loadNamespace` builds, so `ry::setkey` and
/// `asNamespace("ry")` work like they do for an installed package.
const NAMESPACE: &str = r#"
local({
  ns <- new.env(hash = TRUE, parent = .BaseNamespaceEnv)
  info <- new.env(hash = TRUE, parent = baseenv())
  assign(".__NAMESPACE__.", info, envir = ns)
  info$spec <- c(name = "ry", version = "0.0.0")
  info$path <- ""
  info$imports <- list(base = TRUE)
  info$dynlibs <- character()
  info$S3methods <- matrix(NA_character_, 0L, 4L)
  info$lazydata <- new.env(hash = TRUE, parent = baseenv())
  .Internal(registerNamespace("ry", ns))
  ns
})
"#;

/// Exports every function of the namespace and attaches them as `package:ry` at position 2.
const ATTACH: &str = r#"
local({
  ns <- asNamespace("ry")
  functions <- ls(ns)
  exports <- new.env(hash = TRUE, parent = baseenv())
  for (name in functions) assign(name, name, envir = exports)
  assign("exports", exports, envir = ns[[".__NAMESPACE__."]])
  env <- attach(NULL, pos = 2L, name = "package:ry")
  for (name in functions) assign(name, get(name, envir = ns), envir = env)
  invisible(NULL)
})
"#;

/// The user-facing functions, attached as `package:ry` right after the default packages so that
/// `frame` wins over `graphics::frame`, which it still calls when given no arguments.
const PACKAGE: &str = r#"
frame <- .frame_new
as.frame <- function(x) .frame_from_list(x)
is.frame <- function(x) inherits(x, "frame")
frame_type <- function(x) .frame_type(x)
frame_read_csv <- function(path, strings_as_factors = FALSE) .frame_read_csv(path, strings_as_factors)
frame_read_parquet <- function(path) .frame_read_parquet(path)
frame_write_csv <- function(x, path) .frame_write_csv(x, path)
frame_write_parquet <- function(x, path) .frame_write_parquet(x, path)
setkey <- function(x, ...) {
  if (!inherits(x, "frame")) return(data.table::setkey(x, ...))
  columns <- vapply(as.list(substitute(list(...)))[-1L], function(e) {
    if (is.character(e)) e else deparse(e)
  }, "")
  .frame_setkey(substitute(x), columns, parent.frame())
}
setkeyv <- function(x, cols, ...) {
  if (!inherits(x, "frame")) return(data.table::setkeyv(x, cols, ...))
  .frame_setkey(substitute(x), cols, parent.frame())
}
key <- function(x) {
  if (!inherits(x, "frame")) return(data.table::key(x))
  .frame_key(x)
}
copy <- function(x) {
  if (!inherits(x, "frame")) return(data.table::copy(x))
  .frame_copy(x)
}
"#;

/// S3 methods for generics of other packages, registered in their generic's methods table as an
/// installed package's `S3method()` directives are. data.table is not loaded at startup, so its
/// method is registered when it loads, the way vctrs' `s3_register` does it.
const METHODS: &str = r#"
local({
  ns <- asNamespace("ry")
  to_data_frame <- function(x, ...) {
    structure(.frame_to_list(x), class = "data.frame", row.names = .set_row_names(dim(x)[[1L]]))
  }
  environment(to_data_frame) <- ns
  registerS3method("as.data.frame", "frame", to_data_frame, envir = ns)
  to_data_table <- function(x, ...) data.table::setDT(to_data_frame(x))[]
  environment(to_data_table) <- list2env(list(to_data_frame = to_data_frame), parent = ns)
  setHook(packageEvent("data.table", "onLoad"), function(...) {
    registerS3method("as.data.table", "frame", to_data_table, envir = asNamespace("data.table"))
  })
})
"#;

/// Attaches `package:ry` at position 2 of the search path.
pub unsafe fn attach_package() -> R<()> {
    let namespace = crate::session::eval_text(NAMESPACE, values::global_env())?;
    crate::session::eval_text(PACKAGE, namespace)?;
    crate::session::eval_text(ATTACH, values::global_env())?;
    crate::session::eval_text(METHODS, values::global_env())?;
    Ok(())
}

// The object.

thread_local! {
    static TAG: Cell<Option<Sexp>> = const { Cell::new(None) };
}

/// The symbol that tags a frame's external pointer. Symbols are never collected, so caching it
/// is safe.
unsafe fn frame_tag() -> R<Sexp> {
    if let Some(tag) = TAG.with(Cell::get) {
        return Ok(tag);
    }
    let tag = install("ry.frame")?;
    TAG.with(|cell| cell.set(Some(tag)));
    Ok(tag)
}

pub unsafe fn is_frame(object: Sexp) -> bool {
    object.kind() == SexpType::ExternalPtr
        && TAG.with(Cell::get).is_some_and(|tag| object.external_tag() == tag)
        && !object.external_address().is_null()
}

/// Wraps `table` in a new frame object, which owns it until the collector finalizes it.
pub unsafe fn wrap(table: Table) -> R<Sexp> {
    let address = Box::into_raw(Box::new(table)).cast::<c_void>();
    let heap = heap();
    let object = match heap.alloc_external_ptr(address, frame_tag()?, nil()) {
        Ok(object) => object,
        Err(failure) => {
            drop(Box::from_raw(address.cast::<Table>()));
            return Err(failure.into());
        }
    };
    heap.root(object);
    heap.alloc_weak_ref(object, nil(), nil(), finalize as *const () as usize, false)?;
    set_attr(object, symbols().class, scalar_string("frame")?)?;
    Ok(object)
}

unsafe extern "C" fn finalize(object: Sexp) {
    let address = object.external_address();
    if !address.is_null() {
        object.set_external_address(std::ptr::null_mut());
        drop(Box::from_raw(address.cast::<Table>()));
    }
}

/// The table a frame owns.
///
/// # Safety
/// The reference must not outlive the object, and the caller must not hold two of them at once.
#[allow(clippy::mut_from_ref)]
pub unsafe fn table_of<'a>(object: Sexp) -> R<&'a mut Table> {
    if !is_frame(object) {
        return Err(error("not a frame"));
    }
    Ok(&mut *object.external_address().cast::<Table>())
}

unsafe fn table_error(call: Sexp, failure: table::Error) -> Unwind {
    error_in(call, failure.to_string())
}

// Conversion between R vectors and columns.

/// A column as a new R vector. An integer or double column is copied straight into the R
/// allocation, chunk by chunk, with nulls rewritten into R's `NA`.
pub unsafe fn column_value(column: &Column) -> R<Sexp> {
    let series = column.as_materialized_series();
    match series.dtype() {
        DataType::Int32 => {
            let chunked = series.i32().map_err(|failure| error(failure.to_string()))?;
            let vector = values::alloc(SexpType::Integer, series.len())?;
            let target = values::ints_mut(vector);
            let mut offset = 0;
            for array in chunked.downcast_iter() {
                let values = array.values().as_slice();
                target[offset..offset + values.len()].copy_from_slice(values);
                if let Some(validity) = array.validity() {
                    for (index, valid) in validity.iter().enumerate() {
                        if !valid {
                            target[offset + index] = NA_INTEGER;
                        }
                    }
                }
                offset += values.len();
            }
            Ok(vector)
        }
        DataType::Float64 => {
            let chunked = series.f64().map_err(|failure| error(failure.to_string()))?;
            let vector = values::alloc(SexpType::Real, series.len())?;
            let target = values::reals_mut(vector);
            let mut offset = 0;
            for array in chunked.downcast_iter() {
                let values = array.values().as_slice();
                target[offset..offset + values.len()].copy_from_slice(values);
                if let Some(validity) = array.validity() {
                    for (index, valid) in validity.iter().enumerate() {
                        if !valid {
                            target[offset + index] = convert::NA_REAL;
                        }
                    }
                }
                offset += values.len();
            }
            Ok(vector)
        }
        _ => r_vector(convert::to_r(series).map_err(|failure| error(failure.to_string()))?),
    }
}

unsafe fn r_vector(vector: RVector) -> R<Sexp> {
    Ok(match vector {
        RVector::Logical(items) => values::logical_vector(&items)?,
        RVector::Integer(items) => values::integer_vector(&items)?,
        RVector::Double(items) => values::real_vector(&items)?,
        RVector::Character(items) => values::string_vector_na(&items)?,
        RVector::Factor { codes, levels } => {
            let vector = values::integer_vector(&codes)?;
            set_attr(vector, symbols().levels, string_vector(&levels)?)?;
            set_attr(vector, symbols().class, scalar_string("factor")?)?;
            vector
        }
    })
}

/// An R vector as a column's contents, or `None` for a value no column type holds.
unsafe fn column_contents(value: Sexp) -> R<Option<RVector>> {
    let value = values::materialize(value)?;
    Ok(Some(match value.kind() {
        SexpType::Logical => RVector::Logical(values::ints(value).to_vec()),
        SexpType::Integer if values::inherits(value, "factor") => {
            let levels = values::get_attr(value, symbols().levels);
            let levels = (0..length(levels))
                .map(|index| char_str(levels.element(index)).into_owned())
                .collect();
            RVector::Factor {
                codes: values::ints(value).to_vec(),
                levels,
            }
        }
        SexpType::Integer => RVector::Integer(values::ints(value).to_vec()),
        SexpType::Real => RVector::Double(values::reals(value).to_vec()),
        SexpType::String => RVector::Character(values::strings(value)),
        _ => return Ok(None),
    }))
}

/// `value` as a column of `height` rows, recycling a single value.
unsafe fn to_column(name: &str, value: Sexp, height: usize, call: Sexp) -> R<Column> {
    let Some(contents) = column_contents(value)? else {
        return Err(error_in(
            call,
            format!("column `{name}` is a {}, which a frame cannot hold", value.kind().name()),
        ));
    };
    let contents = recycle(contents, height).ok_or_else(|| {
        error_in(
            call,
            format!("column `{name}` has {} rows, but the frame has {height}", length(value)),
        )
    })?;
    convert::to_series(PlSmallStr::from(name), contents)
        .map(IntoColumn::into_column)
        .map_err(|failure| table_error(call, failure))
}

fn recycle(contents: RVector, height: usize) -> Option<RVector> {
    let length = contents.len();
    if length == height {
        return Some(contents);
    }
    if length != 1 {
        return None;
    }
    Some(match contents {
        RVector::Logical(items) => RVector::Logical(vec![items[0]; height]),
        RVector::Integer(items) => RVector::Integer(vec![items[0]; height]),
        RVector::Double(items) => RVector::Double(vec![items[0]; height]),
        RVector::Character(items) => RVector::Character(vec![items[0].clone(); height]),
        RVector::Factor { codes, levels } => RVector::Factor {
            codes: vec![codes[0]; height],
            levels,
        },
    })
}

/// Builds a table from named R vectors, recycling single values to the longest.
unsafe fn table_from_values(columns: &[(String, Sexp)], call: Sexp) -> R<Table> {
    let height = columns
        .iter()
        .map(|(_, value)| length(*value))
        .max()
        .unwrap_or(0);
    let mut converted = Vec::with_capacity(columns.len());
    let mut seen = FxHashSet::default();
    for (name, value) in columns {
        if !seen.insert(name.as_str()) {
            return Err(error_in(call, format!("the column name `{name}` is repeated")));
        }
        converted.push(to_column(name, *value, height, call)?);
    }
    let frame = DataFrame::new(height, converted).map_err(|failure| error_in(call, failure.to_string()))?;
    Table::new(frame).map_err(|failure| table_error(call, failure))
}

/// The columns of a list, a data.frame or a data.table.
unsafe fn table_from_list(list: Sexp, call: Sexp) -> R<Table> {
    if is_frame(list) {
        return Ok(table_of(list)?.fork());
    }
    if list.kind() != SexpType::List {
        return Err(error_in(call, "a frame is built from a list, a data.frame or a data.table"));
    }
    let names = values::names(list);
    let columns: Vec<(String, Sexp)> = (0..length(list))
        .map(|index| {
            let name = if is_nil(names) {
                None
            } else {
                values::string_at(names, index).filter(|name| !name.is_empty())
            };
            let name = name.map_or_else(|| format!("V{}", index + 1), |name| name.into_owned());
            (name, list.element(index))
        })
        .collect();
    table_from_values(&columns, call)
}

// Constructors and IO.

unsafe fn do_frame_new(call: &Call<'_>) -> R<Sexp> {
    let pieces = argument_pieces(call.args, call.env)?;
    if pieces.is_empty() {
        let plot_new = crate::lower::parse_text("graphics::plot.new()", "<frame>")?;
        heap().root(plot_new);
        return eval::eval(plot_new.element(0), call.env);
    }
    let mut columns = Vec::with_capacity(pieces.len());
    for (position, piece) in pieces.iter().enumerate() {
        let value = piece.value()?;
        heap().root(value);
        let name = match &piece.tag {
            Some(tag) => tag.clone(),
            None => match piece.expression() {
                Some(expression) if expression.kind() == SexpType::Symbol => {
                    symbol_name(expression).into_owned()
                }
                _ => format!("V{}", position + 1),
            },
        };
        if piece.tag.is_none() && (value.kind() == SexpType::List || is_frame(value)) {
            let table = table_from_list(value, call.call)?;
            for column in table.frame().columns() {
                columns.push((column.name().to_string(), column_value(column)?));
            }
            continue;
        }
        columns.push((name, value));
    }
    interp().visible = true;
    wrap(table_from_values(&columns, call.call)?)
}

unsafe fn do_from_list(call: &Call<'_>) -> R<Sexp> {
    let value = call.arg(0);
    if is_frame(value) {
        return Ok(value);
    }
    wrap(table_from_list(value, call.call)?)
}

unsafe fn do_to_list(call: &Call<'_>) -> R<Sexp> {
    let table = table_of(call.arg(0))?;
    let columns = table.frame().columns();
    let list = values::alloc(SexpType::List, columns.len())?;
    let mut names = Vec::with_capacity(columns.len());
    for (index, column) in columns.iter().enumerate() {
        values::set_elt(list, index, column_value(column)?);
        names.push(column.name().to_string());
    }
    set_attr(list, symbols().names, string_vector(&names)?)?;
    Ok(list)
}

unsafe fn do_read_csv(call: &Call<'_>) -> R<Sexp> {
    let path = values::require_string(call.arg(0), "path")?;
    let factors = values::require_flag(call.arg(1), "strings_as_factors")?;
    let fail = |failure: &dyn std::fmt::Display| error_in(call.call, format!("cannot read `{path}`: {failure}"));
    let frame = CsvReadOptions::default()
        .with_has_header(true)
        .try_into_reader_with_file_path(Some(path.clone().into()))
        .and_then(|reader| reader.finish())
        .map_err(|failure| fail(&failure))?;
    let height = frame.height();
    let mut columns = Vec::with_capacity(frame.width());
    for column in frame.into_columns() {
        let series = column.as_materialized_series();
        let series = match series.dtype() {
            DataType::String if factors => string_to_factor(series).map_err(|failure| fail(&failure))?,
            // R reads a whole-number column as integer when every value fits, and as double
            // otherwise, as `fread` does without bit64.
            DataType::Int64 => {
                let values = series.i64().map_err(|failure| fail(&failure))?;
                let fits = |value: Option<i64>| {
                    value.is_none_or(|value| value > i64::from(i32::MIN) && value <= i64::from(i32::MAX))
                };
                let target = if fits(values.min()) && fits(values.max()) {
                    DataType::Int32
                } else {
                    DataType::Float64
                };
                series.cast(&target).map_err(|failure| fail(&failure))?
            }
            _ => series.clone(),
        };
        columns.push(series.into_column());
    }
    let frame = DataFrame::new(height, columns).map_err(|failure| fail(&failure))?;
    wrap(Table::new(frame).map_err(|failure| fail(&failure))?)
}

unsafe fn do_read_parquet(call: &Call<'_>) -> R<Sexp> {
    let path = values::require_string(call.arg(0), "path")?;
    let fail = |failure: &dyn std::fmt::Display| error_in(call.call, format!("cannot read `{path}`: {failure}"));
    let file = std::fs::File::open(&path).map_err(|failure| fail(&failure))?;
    let frame = ParquetReader::new(file).finish().map_err(|failure| fail(&failure))?;
    wrap(Table::new(frame).map_err(|failure| fail(&failure))?)
}

unsafe fn do_write_csv(call: &Call<'_>) -> R<Sexp> {
    let table = table_of(call.arg(0))?;
    let path = values::require_string(call.arg(1), "path")?;
    let fail = |failure: &dyn std::fmt::Display| error_in(call.call, format!("cannot write `{path}`: {failure}"));
    let mut file = std::fs::File::create(&path).map_err(|failure| fail(&failure))?;
    let mut frame = table.frame().clone();
    CsvWriter::new(&mut file)
        .finish(&mut frame)
        .map_err(|failure| fail(&failure))?;
    Ok(call.arg(0))
}

unsafe fn do_write_parquet(call: &Call<'_>) -> R<Sexp> {
    let table = table_of(call.arg(0))?;
    let path = values::require_string(call.arg(1), "path")?;
    let fail = |failure: &dyn std::fmt::Display| error_in(call.call, format!("cannot write `{path}`: {failure}"));
    let file = std::fs::File::create(&path).map_err(|failure| fail(&failure))?;
    let mut frame = table.frame().clone();
    ParquetWriter::new(file)
        .finish(&mut frame)
        .map_err(|failure| fail(&failure))?;
    Ok(call.arg(0))
}

unsafe fn do_frame_type(call: &Call<'_>) -> R<Sexp> {
    scalar_string(&table_of(call.arg(0))?.schema().to_string())
}

unsafe fn do_frame_key(call: &Call<'_>) -> R<Sexp> {
    let key = table_of(call.arg(0))?.key();
    if key.is_empty() {
        return Ok(nil());
    }
    string_vector(key)
}

unsafe fn do_frame_copy(call: &Call<'_>) -> R<Sexp> {
    wrap(table_of(call.arg(0))?.fork())
}

/// `.frame_setkey(name, columns, env)`: sorts the frame `name` in `env` by `columns` and rebinds
/// it, so other references keep the unsorted frame.
unsafe fn do_frame_setkey(call: &Call<'_>) -> R<Sexp> {
    let name = call.arg(0);
    let env = call.arg(2);
    if name.kind() != SexpType::Symbol {
        return Err(error_in(call.call, "`setkey` on a frame needs the frame's variable name"));
    }
    let subject = eval::eval(name, env)?;
    let mut table = table_of(subject)?.fork();
    let columns = values::strings(call.arg(1));
    let columns: Vec<&str> = columns.iter().filter_map(|column| column.as_deref()).collect();
    table
        .set_key(&columns)
        .map_err(|failure| table_error(call.call, failure))?;
    let updated = wrap(table)?;
    rebind(name, updated, env)?;
    Ok(updated)
}

/// Rebinds the variable `name` resolves to from `env` to `value`.
unsafe fn rebind(name: Sexp, value: Sexp, env: Sexp) -> R<()> {
    match eval::find_cell(name, env) {
        Some(cell) => heap().set_binding_value(cell, value)?,
        None => eval::define(name, value, env)?,
    }
    Ok(())
}

// Methods.

unsafe fn do_dollar(call: &Call<'_>) -> R<Sexp> {
    let table = table_of(call.arg(0))?;
    let name = values::require_string(call.arg(1), "name")?;
    match table.frame().column(&name) {
        Ok(column) => column_value(column),
        Err(_) => Ok(nil()),
    }
}

unsafe fn do_column(call: &Call<'_>) -> R<Sexp> {
    let (matched, _) = call.match_args(&["x", "i", "exact"])?;
    let subject = matched[0].ok_or_else(|| call.error("argument \"x\" is missing"))?;
    let table = table_of(subject)?;
    let index = matched[1].ok_or_else(|| call.error("argument \"i\" is missing"))?;
    let column = column_at(table, index, call.call)?;
    column_value(column)
}

unsafe fn column_at(table: &Table, index: Sexp, call: Sexp) -> R<&Column> {
    let columns = table.frame().columns();
    match index.kind() {
        SexpType::String => {
            let name = values::require_string(index, "column")?;
            table
                .frame()
                .column(&name)
                .map_err(|_| error_in(call, format!("the frame has no column `{name}`")))
        }
        SexpType::Integer | SexpType::Real => {
            let position = values::as_integer_scalar(index);
            usize::try_from(position)
                .ok()
                .and_then(|position| position.checked_sub(1))
                .and_then(|position| columns.get(position))
                .ok_or_else(|| error_in(call, format!("the frame has no column {position}")))
        }
        _ => Err(error_in(call, "a column is selected by name or by position")),
    }
}

unsafe fn do_set_column(call: &Call<'_>) -> R<Sexp> {
    let mut table = table_of(call.arg(0))?.fork();
    let name = match call.arg(1).kind() {
        SexpType::String => values::require_string(call.arg(1), "name")?,
        _ => column_at(&table, call.arg(1), call.call)?.name().to_string(),
    };
    let value = call.arg(2);
    if is_nil(value) {
        table
            .drop_column(&name)
            .map_err(|failure| table_error(call.call, failure))?;
    } else {
        let column = to_column(&name, value, table.height(), call.call)?;
        table
            .set_column(column)
            .map_err(|failure| table_error(call.call, failure))?;
    }
    wrap(table)
}

unsafe fn do_names(call: &Call<'_>) -> R<Sexp> {
    let names: Vec<String> = table_of(call.arg(0))?
        .frame()
        .get_column_names()
        .iter()
        .map(|name| name.to_string())
        .collect();
    string_vector(&names)
}

unsafe fn do_dim(call: &Call<'_>) -> R<Sexp> {
    let table = table_of(call.arg(0))?;
    values::integer_vector(&[r_count(table.height())?, r_count(table.width())?])
}

unsafe fn do_dimnames(call: &Call<'_>) -> R<Sexp> {
    let names = do_names(call)?;
    list_vector(&[nil(), names])
}

unsafe fn do_length(call: &Call<'_>) -> R<Sexp> {
    scalar_integer(r_count(table_of(call.arg(0))?.width())?)
}

fn r_count(count: usize) -> R<i32> {
    i32::try_from(count).map_err(|_| error("the frame is too large for an R integer"))
}

unsafe fn do_head(call: &Call<'_>) -> R<Sexp> {
    let (matched, _) = call.match_args(&["x", "n", "..."])?;
    let subject = matched[0].ok_or_else(|| call.error("argument \"x\" is missing"))?;
    let count = matched[1].map_or(6, |value| values::as_integer_scalar(value) as i64);
    let table = table_of(subject)?;
    let height = table.height() as i64;
    let count = if count < 0 { (height + count).max(0) } else { count.min(height) };
    let positions: Vec<u32> = (0..count as u32).collect();
    subset_rows(table, positions, call.call)
}

unsafe fn do_tail(call: &Call<'_>) -> R<Sexp> {
    let (matched, _) = call.match_args(&["x", "n", "..."])?;
    let subject = matched[0].ok_or_else(|| call.error("argument \"x\" is missing"))?;
    let count = matched[1].map_or(6, |value| values::as_integer_scalar(value) as i64);
    let table = table_of(subject)?;
    let height = table.height() as i64;
    let count = if count < 0 { (height + count).max(0) } else { count.min(height) };
    let positions: Vec<u32> = ((height - count) as u32..height as u32).collect();
    subset_rows(table, positions, call.call)
}

unsafe fn subset_rows(table: &mut Table, positions: Vec<u32>, call: Sexp) -> R<Sexp> {
    let query = Query {
        rows: Rows::Positions(positions),
        ..Query::default()
    };
    finish(table, query, call)
}

/// `[` reached through dispatch with evaluated arguments, as `do.call("[", ...)` and `lapply`
/// call it: rows by position or logical vector, and columns by name or position.
unsafe fn do_bracket_values(call: &Call<'_>) -> R<Sexp> {
    let Some(subject) = call.args.first().map(|arg| arg.value) else {
        return Err(call.error("argument \"x\" is missing"));
    };
    let table = table_of(subject)?;
    let positional: Vec<Sexp> = call.args[1..]
        .iter()
        .filter(|arg| is_nil(arg.tag))
        .map(|arg| arg.value)
        .collect();
    let mut result = table.fork();
    if let Some(&rows) = positional.first()
        && rows != missing_arg()
    {
        let positions = row_positions(rows, result.height(), call.call)?;
        result = match query::execute(&mut result, Query { rows: Rows::Positions(positions), ..Query::default() })
            .map_err(|failure| table_error(call.call, failure))?
        {
            Output::Table(table) => table,
            _ => return Err(call.error("the row selection did not produce a frame")),
        };
    }
    if let Some(&columns) = positional.get(1)
        && columns != missing_arg()
    {
        let names = column_names(&result, columns, call.call)?;
        let select = Select::Columns(names.iter().map(|name| (name.clone(), Expr::Column(name.clone()))).collect());
        return finish(&mut result, Query { select, ..Query::default() }, call.call);
    }
    wrap(result)
}

unsafe fn column_names(table: &Table, selection: Sexp, call: Sexp) -> R<Vec<PlSmallStr>> {
    let schema = table.schema();
    match selection.kind() {
        SexpType::String => values::strings(selection)
            .into_iter()
            .map(|name| {
                let name = name.unwrap_or_default();
                match schema.column(&name) {
                    Some(_) => Ok(PlSmallStr::from(name)),
                    None => Err(error_in(call, format!("the frame has no column `{name}`"))),
                }
            })
            .collect(),
        SexpType::Integer | SexpType::Real => {
            let positions = row_positions(selection, schema.columns.len(), call)?;
            Ok(positions
                .into_iter()
                .filter_map(|position| schema.columns.get(position as usize))
                .map(|(name, _)| name.clone())
                .collect())
        }
        SexpType::Logical => {
            let positions = row_positions(selection, schema.columns.len(), call)?;
            Ok(positions
                .into_iter()
                .filter_map(|position| schema.columns.get(position as usize))
                .map(|(name, _)| name.clone())
                .collect())
        }
        _ => Err(error_in(call, "columns are selected by name or by position")),
    }
}

/// Zero-based positions from an R index: logical (recycled, `NA` dropping the row), or numeric,
/// one-based, where negative positions exclude and zero selects nothing.
unsafe fn row_positions(index: Sexp, height: usize, call: Sexp) -> R<Vec<u32>> {
    let index = values::materialize(index)?;
    match index.kind() {
        SexpType::Logical => {
            let flags = values::ints(index);
            if flags.is_empty() {
                return Ok(Vec::new());
            }
            if flags.len() != height && flags.len() != 1 {
                return Err(error_in(
                    call,
                    format!("the row selection has {} values, but the frame has {height} rows", flags.len()),
                ));
            }
            Ok((0..height)
                .filter(|&row| flags[row % flags.len()] == 1)
                .map(|row| row as u32)
                .collect())
        }
        SexpType::Integer | SexpType::Real => {
            let numbers: Vec<f64> = if index.kind() == SexpType::Integer {
                values::ints(index)
                    .iter()
                    .map(|&value| if value == NA_INTEGER { f64::NAN } else { f64::from(value) })
                    .collect()
            } else {
                values::reals(index).to_vec()
            };
            if numbers.iter().any(|&value| value < 0.0) {
                if numbers.iter().any(|&value| value > 0.0 || value.is_nan()) {
                    return Err(error_in(call, "cannot mix positive and negative row positions"));
                }
                let mut keep = vec![true; height];
                for value in numbers {
                    let position = (-value) as usize;
                    if position >= 1 && position <= height {
                        keep[position - 1] = false;
                    }
                }
                return Ok((0..height).filter(|&row| keep[row]).map(|row| row as u32).collect());
            }
            let mut positions = Vec::with_capacity(numbers.len());
            for value in numbers {
                if value.is_nan() || value as usize > height {
                    return Err(error_in(
                        call,
                        format!("row {} is out of range for a frame of {height} rows", format_position(value)),
                    ));
                }
                if value >= 1.0 {
                    positions.push(value as u32 - 1);
                }
            }
            Ok(positions)
        }
        _ => Err(error_in(call, "rows are selected by a logical or numeric vector")),
    }
}

fn format_position(value: f64) -> String {
    if value.is_nan() { "NA".to_string() } else { format!("{value}") }
}

// Printing.

unsafe fn do_print(call: &Call<'_>) -> R<Sexp> {
    let subject = call.arg(0);
    let table = table_of(subject)?;
    let text = render(table)?;
    write_console(text.as_bytes(), false);
    Ok(subject)
}

/// The frame as data.table prints a table: a header, a row of column types, and numbered rows,
/// with only the first and last five rows of a frame longer than a hundred.
unsafe fn render(table: &mut Table) -> R<String> {
    let height = table.height();
    let width = table.width();
    if width == 0 || height == 0 {
        let names: Vec<String> = table.frame().get_column_names().iter().map(|name| name.to_string()).collect();
        let mut text = format!("Empty frame ({height} rows and {width} cols)");
        if !names.is_empty() {
            text.push_str(": ");
            text.push_str(&names.join(","));
        }
        text.push('\n');
        return Ok(text);
    }
    let (positions, split) = if height > 100 {
        let mut positions: Vec<u32> = (0..5).collect();
        positions.extend(height as u32 - 5..height as u32);
        (positions, Some(5))
    } else {
        ((0..height as u32).collect(), None)
    };
    let shown = match query::execute(table, Query { rows: Rows::Positions(positions.clone()), ..Query::default() })
        .map_err(|failure| error(failure.to_string()))?
    {
        Output::Table(shown) => shown,
        _ => return Err(error("printing a frame did not select rows")),
    };
    let labels: Vec<String> = positions.iter().map(|position| format!("{}:", position + 1)).collect();
    let separator_width = if split.is_some() { 3 } else { 0 };
    let label_width = labels.iter().map(|label| label.len()).max().unwrap_or(0).max(separator_width);
    let mut lines: Vec<String> = vec![" ".repeat(label_width), " ".repeat(label_width)];
    for label in &labels {
        lines.push(format!("{label:>label_width$}"));
    }
    let schema = shown.schema();
    for (column, (name, kind)) in shown.frame().columns().iter().zip(&schema.columns) {
        let cells = format_cells(column, *kind)?;
        let type_label = match kind {
            ColumnType::Logical => "<lgcl>",
            ColumnType::Integer => "<int>",
            ColumnType::Double => "<num>",
            ColumnType::Character => "<char>",
            ColumnType::Factor => "<fctr>",
        };
        let cell_width = cells
            .iter()
            .map(|cell| display_width(cell))
            .chain([display_width(name), type_label.len()])
            .max()
            .unwrap_or(0);
        let pad = |text: &str| format!("{}{text}", " ".repeat(cell_width - display_width(text)));
        lines[0].push(' ');
        lines[0].push_str(&pad(name));
        lines[1].push(' ');
        lines[1].push_str(&pad(type_label));
        for (row, cell) in cells.iter().enumerate() {
            lines[row + 2].push(' ');
            lines[row + 2].push_str(&pad(cell));
        }
    }
    if let Some(split) = split {
        lines.insert(split + 2, format!("{:>label_width$}", "---"));
    }
    let mut text = lines.join("\n");
    text.push('\n');
    Ok(text)
}

unsafe fn format_cells(column: &Column, kind: ColumnType) -> R<Vec<String>> {
    let vector = column_value(column)?;
    Ok(match kind {
        ColumnType::Double => {
            let data = values::reals(vector);
            let settings = Settings {
                na_width: 2,
                ..Settings::current()
            };
            let format = format_real_common(data, settings);
            data.iter().map(|&value| format_real_with(value, format, "NA")).collect()
        }
        ColumnType::Integer => values::ints(vector)
            .iter()
            .map(|&value| if value == NA_INTEGER { "NA".to_string() } else { value.to_string() })
            .collect(),
        ColumnType::Logical => values::ints(vector)
            .iter()
            .map(|&value| match value {
                NA_INTEGER => "NA".to_string(),
                0 => "FALSE".to_string(),
                _ => "TRUE".to_string(),
            })
            .collect(),
        ColumnType::Character => values::strings(vector)
            .into_iter()
            .map(|value| value.unwrap_or_else(|| "<NA>".to_string()))
            .collect(),
        ColumnType::Factor => {
            let levels = values::strings(values::get_attr(vector, symbols().levels));
            values::ints(vector)
                .iter()
                .map(|&code| {
                    usize::try_from(code)
                        .ok()
                        .and_then(|code| code.checked_sub(1))
                        .and_then(|code| levels.get(code).cloned().flatten())
                        .unwrap_or_else(|| "<NA>".to_string())
                })
                .collect()
        }
    })
}

// Queries.

/// One argument of a query call, unevaluated: its expression and the environment to evaluate it
/// in, or a value already forced (a promise from `...` that something else forced).
struct Piece {
    tag: Option<String>,
    source: Source,
}

enum Source {
    Expression(Sexp, Sexp),
    /// An unforced promise from `...`, forced (once) when its value is needed.
    Promise(Sexp),
    Value(Sexp),
    Missing,
}

impl Piece {
    unsafe fn value(&self) -> R<Sexp> {
        match self.source {
            Source::Expression(expression, env) => eval::eval(expression, env),
            Source::Promise(promise) => eval::force_promise(promise),
            Source::Value(value) => Ok(value),
            Source::Missing => Ok(missing_arg()),
        }
    }

    fn expression(&self) -> Option<Sexp> {
        match self.source {
            Source::Expression(expression, _) => Some(expression),
            Source::Promise(promise) => Some(unsafe { promise.cdr() }),
            _ => None,
        }
    }

    fn is_missing(&self) -> bool {
        matches!(self.source, Source::Missing)
    }
}

/// The arguments of a call as pieces, expanding `...` into the promises it holds.
unsafe fn argument_pieces(args: &[Arg], env: Sexp) -> R<Vec<Piece>> {
    let dots = symbols().dots;
    let mut pieces = Vec::with_capacity(args.len());
    for arg in args {
        if arg.value == dots {
            let value = eval::find_var(dots, env)?;
            if value.kind() == SexpType::Dots {
                let mut cell = value;
                while matches!(cell.kind(), SexpType::Dots | SexpType::Pairlist) {
                    pieces.push(Piece {
                        tag: tag_name(cell.tag()),
                        source: promise_source(cell.car()),
                    });
                    cell = cell.cdr();
                }
            }
            continue;
        }
        let source = if arg.value == missing_arg() {
            Source::Missing
        } else {
            Source::Expression(arg.value, env)
        };
        pieces.push(Piece {
            tag: tag_name(arg.tag),
            source,
        });
    }
    Ok(pieces)
}

unsafe fn promise_source(mut item: Sexp) -> Source {
    if item == missing_arg() {
        return Source::Missing;
    }
    loop {
        if item.kind() != SexpType::Promise {
            return Source::Value(item);
        }
        if item.car() != values::unbound() {
            return Source::Value(item.car());
        }
        if item.cdr().kind() == SexpType::Promise {
            item = item.cdr();
            continue;
        }
        return Source::Promise(item);
    }
}

unsafe fn tag_name(tag: Sexp) -> Option<String> {
    (!is_nil(tag) && tag.kind() == SexpType::Symbol).then(|| symbol_name(tag).into_owned())
}

/// What [`intercept`] did with a `[` call.
pub enum Intercepted {
    Value(Sexp),
    Args(Vec<Arg>),
}

/// Evaluates a `[` call's subject once, then hands the call to [`query`] when it is a frame or to
/// an S3 method with the other arguments unevaluated, and otherwise returns every argument
/// evaluated for the `[` primitive.
pub unsafe fn intercept(call: Sexp, arguments: Sexp, env: Sexp, keep_missing: bool) -> R<Intercepted> {
    let first = arguments.car();
    if arguments.kind() != SexpType::Pairlist || first == symbols().dots || first == missing_arg() {
        return eval::evaluate_args(arguments, env, keep_missing, call).map(Intercepted::Args);
    }
    let subject = eval::eval(first, env)?;
    if is_frame(subject) {
        return run(call, subject, env).map(Intercepted::Value);
    }
    if let Some(value) = crate::dispatch::dispatch_subset(call, arguments, subject, env)? {
        return Ok(Intercepted::Value(value));
    }
    let mut args = vec![Arg {
        tag: arguments.tag(),
        value: subject,
    }];
    args.extend(eval::evaluate_args(arguments.cdr(), env, keep_missing, call)?);
    Ok(Intercepted::Args(args))
}

/// Runs a query in its own handle scope, signals an error it raised, and sets the visibility.
pub unsafe fn run(call: Sexp, subject: Sexp, env: Sexp) -> R<Sexp> {
    let result = scope(|| query(call, subject, env));
    match result {
        Err(Unwind::Error(failure)) => Err(conditions::signal_error(failure)),
        result => result,
    }
}

/// The parts of a `[` call on a frame.
#[derive(Default)]
struct Parts {
    i: Option<Piece>,
    j: Option<Piece>,
    by: Option<Piece>,
    keyed: bool,
    on: Option<Piece>,
    nomatch: Option<Piece>,
    roll: Option<Piece>,
    mult: Option<Piece>,
    sdcols: Option<Piece>,
    with: Option<Piece>,
}

/// Runs `subject[i, j, by]`, where `call` is the whole `[` call as written.
pub unsafe fn query(call: Sexp, subject: Sexp, env: Sexp) -> R<Sexp> {
    let args: Vec<Arg> = values::pairlist_items(call.cdr().cdr())
        .into_iter()
        .map(|(tag, value)| Arg { tag, value })
        .collect();
    let pieces = argument_pieces(&args, env)?;
    let mut parts = Parts::default();
    let mut positional = 0;
    for piece in pieces {
        match piece.tag.as_deref() {
            None | Some("") => {
                let slot = match positional {
                    0 => &mut parts.i,
                    1 => &mut parts.j,
                    2 => &mut parts.by,
                    _ => return Err(error_in(call, "a frame query takes at most `i`, `j` and `by`")),
                };
                positional += 1;
                if !piece.is_missing() {
                    *slot = Some(piece);
                }
            }
            Some(name) => {
                let slot = match name {
                    "i" => &mut parts.i,
                    "j" => &mut parts.j,
                    "by" => &mut parts.by,
                    "keyby" => {
                        parts.keyed = true;
                        &mut parts.by
                    }
                    "on" => &mut parts.on,
                    "nomatch" => &mut parts.nomatch,
                    "roll" => &mut parts.roll,
                    "mult" => &mut parts.mult,
                    ".SDcols" => &mut parts.sdcols,
                    "with" => &mut parts.with,
                    "drop" | "verbose" | "allow.cartesian" => continue,
                    other => {
                        return Err(error_in(call, format!("frame queries do not support `{other} =`")));
                    }
                };
                if !piece.is_missing() {
                    *slot = Some(piece);
                }
            }
        }
    }
    let table = table_of(subject)?;
    let schema = table.schema();
    let lowering = Lowering {
        schema: &schema,
        height: table.height(),
        env,
        call,
    };
    let by = match &parts.by {
        Some(piece) => lowering.by(piece)?,
        None => Some(Vec::new()),
    };
    let rows = match &parts.i {
        Some(piece) => lowering.rows(piece, &parts)?,
        None => Some(Rows::All),
    };
    let select = match &parts.j {
        Some(piece) => lowering.select(piece, &parts, &by)?,
        None => Some(Select::All),
    };
    let assigns = matches!(select, Some(Select::Assign(_)))
        || parts
            .j
            .as_ref()
            .and_then(Piece::expression)
            .is_some_and(|expression| is_call_to(expression, ":="));
    // The engine updates through a logical filter or every row; any other row selection under
    // `:=` (positions, an order, a join) takes the R path.
    let lowered_update = !assigns || matches!(rows, Some(Rows::All | Rows::Filter(_)));
    let result = match (rows, select, by) {
        (Some(rows), Some(select), Some(by)) if lowered_update => {
            let query = Query {
                rows,
                select,
                by,
                keyed: parts.keyed,
            };
            if assigns {
                assign(subject, table, call, env, |table| {
                    query::execute(table, query).map(|_| ()).map_err(|failure| table_error(call, failure))
                })?
            } else {
                finish(table, query, call)?
            }
        }
        (rows, select, by) => fallback(subject, table, &parts, rows, select, by, &lowering)?,
    };
    interp().visible = !assigns;
    Ok(result)
}

/// Runs a query that does not assign and wraps what it produced.
unsafe fn finish(table: &mut Table, query: Query, call: Sexp) -> R<Sexp> {
    match query::execute(table, query).map_err(|failure| table_error(call, failure))? {
        Output::Table(result) => wrap(result),
        Output::Vector(series) => column_value(&series.into_column()),
        Output::Updated => Err(error_in(call, "the query updated the frame unexpectedly")),
    }
}

/// Applies an update to `subject`, whose table is `table`, under copy-on-modify: in place when
/// the variable the call names is the frame's only reference, and otherwise to a fork, which then
/// replaces that variable.
///
/// The subject must be a variable. A frame reached through an expression such as `lst$a` has no
/// binding to rebind, and its single reference may be a slot in a list that is itself shared, so
/// updating it in place would change every copy of the list.
unsafe fn assign(
    subject: Sexp,
    table: &mut Table,
    call: Sexp,
    env: Sexp,
    update: impl FnOnce(&mut Table) -> R<()>,
) -> R<Sexp> {
    let name = call.cdr().car();
    if name.kind() != SexpType::Symbol {
        return Err(error_in(
            call,
            "`:=` updates a frame stored in a variable; assign it to one first, as in `y <- lst$a; y[, z := 1]`",
        ));
    }
    if subject.refcount() <= 1 {
        update(table)?;
        return Ok(subject);
    }
    let mut fork = table.fork();
    update(&mut fork)?;
    let updated = wrap(fork)?;
    rebind(name, updated, env)?;
    Ok(updated)
}

/// Lowers R expressions into the query IR. Every method returns `None` for an expression the IR
/// cannot express, which then runs as R code.
struct Lowering<'a> {
    schema: &'a table::Schema,
    /// The queried frame's height, which numeric and logical row indices resolve against.
    height: usize,
    env: Sexp,
    call: Sexp,
}

impl Lowering<'_> {
    fn is_column(&self, name: &str) -> bool {
        self.schema.column(name).is_some()
    }

    unsafe fn rows(&self, piece: &Piece, parts: &Parts) -> R<Option<Rows>> {
        let Some(expression) = piece.expression() else {
            return self.rows_from_value(piece.value()?, parts).map(Some);
        };
        if is_call_to(expression, "order") {
            let mut keys = Vec::new();
            for (_, argument) in values::pairlist_items(expression.cdr()) {
                let (argument, descending) = if is_call_to(argument, "-") && length(argument) == 2 {
                    (argument.cdr().car(), true)
                } else {
                    (argument, false)
                };
                let Some(expr) = self.expr(argument)? else {
                    return Ok(None);
                };
                keys.push(SortKey { expr, descending });
            }
            return Ok(Some(Rows::Order(keys)));
        }
        if !self.mentions_columns(expression) {
            let value = eval::eval(expression, self.env)?;
            return self.rows_from_value(value, parts).map(Some);
        }
        if !self.mentions(expression, false) {
            // Only `.N` and its kin, as in `x[.N]` or `x[.N - 1]`: a row number, not a filter.
            let mask = values::new_env(self.env)?;
            eval::define(install(".N")?, scalar_integer(r_count(self.height)?)?, mask)?;
            let value = eval::eval(expression, mask)?;
            return self.rows_from_value(value, parts).map(Some);
        }
        Ok(self.expr(expression)?.map(Rows::Filter))
    }

    unsafe fn rows_from_value(&self, value: Sexp, parts: &Parts) -> R<Rows> {
        if is_frame(value) || value.kind() == SexpType::List {
            let other = if is_frame(value) {
                table_of(value)?.fork()
            } else {
                table_from_list(value, self.call)?
            };
            return self.join(other, parts).map(Rows::Join);
        }
        if parts.on.is_some() {
            return Err(error_in(self.call, "a join with `on =` needs a frame as `i`"));
        }
        row_positions(value, self.height, self.call).map(Rows::Positions)
    }

    unsafe fn join(&self, other: Table, parts: &Parts) -> R<Join> {
        let on = match &parts.on {
            Some(piece) => self.on(piece)?,
            None => Vec::new(),
        };
        let drop_unmatched = match &parts.nomatch {
            Some(piece) => {
                let value = piece.value()?;
                is_nil(value) || (length(value) == 1 && values::as_integer_scalar(value) == 0)
            }
            None => false,
        };
        let roll = match &parts.roll {
            Some(piece) => roll(piece.value()?, self.call)?,
            None => Roll::None,
        };
        let mult = match &parts.mult {
            Some(piece) => match values::first_string(piece.value()?).as_deref() {
                Some("all") => Mult::All,
                Some("first") => Mult::First,
                Some("last") => Mult::Last,
                _ => return Err(error_in(self.call, "`mult =` takes \"all\", \"first\" or \"last\"")),
            },
            None => Mult::All,
        };
        Ok(Join {
            other,
            on,
            drop_unmatched,
            roll,
            mult,
        })
    }

    unsafe fn on(&self, piece: &Piece) -> R<Vec<(PlSmallStr, PlSmallStr)>> {
        if let Some(expression) = piece.expression()
            && (is_call_to(expression, ".") || is_call_to(expression, "list"))
        {
            let mut pairs = Vec::new();
            for (tag, value) in values::pairlist_items(expression.cdr()) {
                let other = match value.kind() {
                    SexpType::Symbol => symbol_name(value).into_owned(),
                    SexpType::String => values::require_string(value, "on")?,
                    _ => return Err(error_in(self.call, "`on =` takes column names")),
                };
                let own = tag_name(tag).unwrap_or_else(|| other.clone());
                pairs.push((PlSmallStr::from(own), PlSmallStr::from(other)));
            }
            return Ok(pairs);
        }
        let value = piece.value()?;
        if value.kind() != SexpType::String {
            return Err(error_in(self.call, "`on =` takes column names"));
        }
        let names = values::names(value);
        let mut pairs = Vec::new();
        for (index, text) in values::strings(value).into_iter().enumerate() {
            let text = text.unwrap_or_default();
            let tag = if is_nil(names) {
                None
            } else {
                values::string_at(names, index).filter(|name| !name.is_empty()).map(|name| name.into_owned())
            };
            for part in text.split(',') {
                let part = part.trim();
                let (own, other) = match (tag.as_deref(), part.split_once("==")) {
                    (_, Some((own, other))) => (own.trim().to_string(), other.trim().to_string()),
                    (Some(tag), None) => (tag.to_string(), part.to_string()),
                    (None, None) => (part.to_string(), part.to_string()),
                };
                pairs.push((PlSmallStr::from(own), PlSmallStr::from(other)));
            }
        }
        Ok(pairs)
    }

    unsafe fn select(
        &self,
        piece: &Piece,
        parts: &Parts,
        by: &Option<Vec<(PlSmallStr, Expr)>>,
    ) -> R<Option<Select>> {
        let with = match &parts.with {
            Some(piece) => values::as_logical_scalar(piece.value()?) != 0,
            None => true,
        };
        let Some(expression) = piece.expression() else {
            return self.columns_from_value(piece.value()?).map(Some);
        };
        if !with {
            let value = eval::eval(expression, self.env)?;
            return self.columns_from_value(value).map(Some);
        }
        if is_call_to(expression, ":=") {
            return self.assignments(expression);
        }
        if is_call_to(expression, ".") || is_call_to(expression, "list") {
            let mut columns = Vec::new();
            for (position, (tag, item)) in values::pairlist_items(expression.cdr()).into_iter().enumerate() {
                let name = match tag_name(tag) {
                    Some(name) => name,
                    None if item.kind() == SexpType::Symbol && self.is_column(&symbol_name(item)) => {
                        symbol_name(item).into_owned()
                    }
                    None => format!("V{}", position + 1),
                };
                let Some(expr) = self.expr(item)? else {
                    return Ok(None);
                };
                columns.push((PlSmallStr::from(name), expr));
            }
            return Ok(Some(Select::Columns(columns)));
        }
        if is_call_to(expression, "lapply") && length(expression) >= 3 {
            let target = expression.cdr().car();
            let function = expression.cdr().cdr().car();
            if target.kind() == SexpType::Symbol && symbol_name(target) == ".SD" && function.kind() == SexpType::Symbol {
                let Some(aggregate) = aggregate_named(&symbol_name(function)) else {
                    return Ok(None);
                };
                let mut na_rm = false;
                for (tag, value) in values::pairlist_items(expression.cdr().cdr().cdr()) {
                    if tag_name(tag).as_deref() == Some("na.rm") {
                        na_rm = values::as_logical_scalar(eval::eval(value, self.env)?) == 1;
                    } else {
                        return Ok(None);
                    }
                }
                let Some(by) = by else {
                    return Ok(None);
                };
                let columns = self
                    .sd_columns(parts, by)?
                    .into_iter()
                    .map(|name| {
                        let arg = Box::new(Expr::Column(name.clone()));
                        (name, Expr::Aggregate { function: aggregate, arg, na_rm })
                    })
                    .collect();
                return Ok(Some(Select::Columns(columns)));
            }
            return Ok(None);
        }
        if expression.kind() == SexpType::Symbol {
            let name = symbol_name(expression);
            if let Some(rest) = name.strip_prefix("..")
                && !rest.is_empty()
                && !rest.starts_with('.')
            {
                let value = eval::eval(install(rest)?, self.env)?;
                return self.columns_from_value(value).map(Some);
            }
            if name == ".SD" {
                let Some(by) = by else {
                    return Ok(None);
                };
                let columns = self
                    .sd_columns(parts, by)?
                    .into_iter()
                    .map(|name| (name.clone(), Expr::Column(name)))
                    .collect();
                return Ok(Some(Select::Columns(columns)));
            }
        }
        if !self.mentions_columns(expression) {
            let value = eval::eval(expression, self.env)?;
            if matches!(value.kind(), SexpType::String | SexpType::Integer | SexpType::Real)
                && !values::inherits(value, "factor")
            {
                return self.columns_from_value(value).map(Some);
            }
            return Ok(literal(value).map(Select::Value));
        }
        Ok(self.expr(expression)?.map(Select::Value))
    }

    unsafe fn columns_from_value(&self, value: Sexp) -> R<Select> {
        let height = self.schema.columns.len();
        let names: Vec<PlSmallStr> = match value.kind() {
            SexpType::String => values::strings(value)
                .into_iter()
                .map(|name| {
                    let name = name.unwrap_or_default();
                    if self.is_column(&name) {
                        Ok(PlSmallStr::from(name))
                    } else {
                        Err(error_in(self.call, format!("the frame has no column `{name}`")))
                    }
                })
                .collect::<R<_>>()?,
            SexpType::Integer | SexpType::Real | SexpType::Logical => row_positions(value, height, self.call)?
                .into_iter()
                .filter_map(|position| self.schema.columns.get(position as usize))
                .map(|(name, _)| name.clone())
                .collect(),
            _ => return Err(error_in(self.call, "`j` selects columns by name or by position here")),
        };
        Ok(Select::Columns(names.into_iter().map(|name| (name.clone(), Expr::Column(name))).collect()))
    }

    /// The columns `.SD` covers: `.SDcols`, or every column that is not a group.
    unsafe fn sd_columns(&self, parts: &Parts, by: &[(PlSmallStr, Expr)]) -> R<Vec<PlSmallStr>> {
        let Some(piece) = &parts.sdcols else {
            return Ok(self
                .schema
                .columns
                .iter()
                .map(|(name, _)| name.clone())
                .filter(|name| !by.iter().any(|(group, _)| group == name))
                .collect());
        };
        if let Some(expression) = piece.expression()
            && let Some(range) = self.column_range(expression)
        {
            return Ok(range);
        }
        match self.columns_from_value(piece.value()?)? {
            Select::Columns(columns) => Ok(columns.into_iter().map(|(name, _)| name).collect()),
            _ => Err(error_in(self.call, "`.SDcols` names columns")),
        }
    }

    /// `a:b` between two column names, which data.table reads as the columns from `a` to `b`.
    unsafe fn column_range(&self, expression: Sexp) -> Option<Vec<PlSmallStr>> {
        if !is_call_to(expression, ":") || length(expression) != 3 {
            return None;
        }
        let (from, to) = (expression.cdr().car(), expression.cdr().cdr().car());
        if from.kind() != SexpType::Symbol || to.kind() != SexpType::Symbol {
            return None;
        }
        let position = |name: &str| self.schema.columns.iter().position(|(column, _)| column == name);
        let (from, to) = (position(&symbol_name(from))?, position(&symbol_name(to))?);
        let (from, to) = if from <= to { (from, to) } else { (to, from) };
        Some(self.schema.columns[from..=to].iter().map(|(name, _)| name.clone()).collect())
    }

    unsafe fn assignments(&self, expression: Sexp) -> R<Option<Select>> {
        let items = values::pairlist_items(expression.cdr());
        let mut pairs: Vec<(String, Sexp)> = Vec::new();
        if items.iter().all(|(tag, _)| !is_nil(*tag)) {
            for (tag, value) in items {
                pairs.push((symbol_name(tag).into_owned(), value));
            }
        } else if items.len() == 2 {
            let (left, right) = (items[0].1, items[1].1);
            let names = match left.kind() {
                SexpType::Symbol => vec![symbol_name(left).into_owned()],
                _ => {
                    let value = eval::eval(left, self.env)?;
                    if value.kind() != SexpType::String {
                        return Err(error_in(self.call, "the left side of `:=` names columns"));
                    }
                    values::strings(value).into_iter().map(Option::unwrap_or_default).collect()
                }
            };
            if names.len() == 1 {
                pairs.push((names[0].clone(), right));
            } else if is_call_to(right, "list") || is_call_to(right, ".") {
                let values = values::pairlist_items(right.cdr());
                if values.len() != names.len() {
                    return Err(error_in(
                        self.call,
                        format!("`:=` names {} columns but gives {} values", names.len(), values.len()),
                    ));
                }
                pairs.extend(names.into_iter().zip(values.into_iter().map(|(_, value)| value)));
            } else {
                return Ok(None);
            }
        } else {
            return Err(error_in(self.call, "`:=` takes `name := value` or named arguments"));
        }
        let mut assignments = Vec::with_capacity(pairs.len());
        for (name, value) in pairs {
            let lowered = if is_nil(value) {
                None
            } else {
                match self.expr(value)? {
                    Some(expr) => Some(expr),
                    None => return Ok(None),
                }
            };
            assignments.push((PlSmallStr::from(name), lowered));
        }
        Ok(Some(Select::Assign(assignments)))
    }

    /// `by` or `keyby`, or `None` when a group is computed by an expression the IR lacks.
    unsafe fn by(&self, piece: &Piece) -> R<Option<Vec<(PlSmallStr, Expr)>>> {
        if let Some(expression) = piece.expression() {
            if expression.kind() == SexpType::Symbol && self.is_column(&symbol_name(expression)) {
                let name = symbol_name(expression).into_owned();
                return Ok(Some(vec![(PlSmallStr::from(name.as_str()), Expr::Column(name.into()))]));
            }
            if let Some(range) = self.column_range(expression) {
                return Ok(Some(range.into_iter().map(|name| (name.clone(), Expr::Column(name))).collect()));
            }
            if is_call_to(expression, ".") || is_call_to(expression, "list") {
                let mut groups = Vec::new();
                for (position, (tag, item)) in values::pairlist_items(expression.cdr()).into_iter().enumerate() {
                    let name = match tag_name(tag) {
                        Some(name) => name,
                        None if item.kind() == SexpType::Symbol => symbol_name(item).into_owned(),
                        None => format!("V{}", position + 1),
                    };
                    let Some(expr) = self.expr(item)? else {
                        return Ok(None);
                    };
                    groups.push((PlSmallStr::from(name), expr));
                }
                return Ok(Some(groups));
            }
            if self.mentions_columns(expression) {
                return Ok(None);
            }
        }
        let value = piece.value()?;
        if value.kind() != SexpType::String {
            return Err(error_in(self.call, "`by` names columns"));
        }
        let mut groups = Vec::new();
        for text in values::strings(value) {
            for name in text.unwrap_or_default().split(',') {
                let name = name.trim();
                if !self.is_column(name) {
                    return Err(error_in(self.call, format!("the frame has no column `{name}`")));
                }
                groups.push((PlSmallStr::from(name), Expr::Column(name.into())));
            }
        }
        Ok(Some(groups))
    }

    /// Lowers an expression, or returns `None` when the IR cannot express it.
    unsafe fn expr(&self, expression: Sexp) -> R<Option<Expr>> {
        match expression.kind() {
            SexpType::Symbol => {
                let name = symbol_name(expression);
                if name == ".N" {
                    return Ok(Some(Expr::Count));
                }
                if self.is_column(&name) {
                    return Ok(Some(Expr::Column(PlSmallStr::from(name.as_ref()))));
                }
                if name.starts_with(".SD") || matches!(name.as_ref(), ".I" | ".GRP" | ".BY" | ".NGRP") {
                    return Ok(None);
                }
                let lookup = match name.strip_prefix("..") {
                    Some(rest) if !rest.is_empty() && !rest.starts_with('.') => install(rest)?,
                    _ => expression,
                };
                Ok(literal(eval::eval(lookup, self.env)?))
            }
            SexpType::Language => {
                if !self.mentions_columns(expression) {
                    return Ok(literal(eval::eval(expression, self.env)?));
                }
                self.call_expr(expression)
            }
            _ => Ok(literal(expression)),
        }
    }

    unsafe fn call_expr(&self, expression: Sexp) -> R<Option<Expr>> {
        let head = expression.car();
        if head.kind() != SexpType::Symbol {
            return Ok(None);
        }
        let name = symbol_name(head);
        let items = values::pairlist_items(expression.cdr());
        let positional: Vec<Sexp> = items.iter().filter(|(tag, _)| is_nil(*tag)).map(|(_, value)| *value).collect();
        let unary = |op: UnaryOp, this: &Self| -> R<Option<Expr>> {
            Ok(this.expr(positional[0])?.map(|inner| Expr::Unary(op, Box::new(inner))))
        };
        match (name.as_ref(), positional.len(), items.len()) {
            ("(", 1, 1) => self.expr(positional[0]),
            ("-", 1, 1) => unary(UnaryOp::Negate, self),
            ("+", 1, 1) => self.expr(positional[0]),
            ("!", 1, 1) => unary(UnaryOp::Not, self),
            ("is.na", 1, 1) => unary(UnaryOp::IsNa, self),
            (operator, 2, 2) if binary_op(operator).is_some() => {
                let Some(op) = binary_op(operator) else {
                    return Ok(None);
                };
                let (Some(left), Some(right)) = (self.expr(positional[0])?, self.expr(positional[1])?) else {
                    return Ok(None);
                };
                Ok(Some(Expr::Binary(op, Box::new(left), Box::new(right))))
            }
            ("head", 2, 2) => {
                let count = eval::eval(positional[1], self.env)?;
                let count = values::as_integer_scalar(count);
                let Ok(count) = usize::try_from(count) else {
                    return Ok(None);
                };
                Ok(self.expr(positional[0])?.map(|inner| Expr::Head(Box::new(inner), count)))
            }
            ("cor", 2, 2) => {
                let (Some(left), Some(right)) = (self.expr(positional[0])?, self.expr(positional[1])?) else {
                    return Ok(None);
                };
                Ok(Some(Expr::Correlation(Box::new(left), Box::new(right))))
            }
            ("ifelse" | "fifelse", 3, 3) => {
                let (Some(test), Some(yes), Some(no)) =
                    (self.expr(positional[0])?, self.expr(positional[1])?, self.expr(positional[2])?)
                else {
                    return Ok(None);
                };
                Ok(Some(Expr::IfElse(Box::new(test), Box::new(yes), Box::new(no))))
            }
            (function, 1, _) if aggregate_named(function).is_some() => {
                let Some(aggregate) = aggregate_named(function) else {
                    return Ok(None);
                };
                let mut na_rm = false;
                for (tag, value) in &items {
                    if is_nil(*tag) {
                        continue;
                    }
                    if tag_name(*tag).as_deref() != Some("na.rm") {
                        return Ok(None);
                    }
                    na_rm = values::as_logical_scalar(eval::eval(*value, self.env)?) == 1;
                }
                Ok(self.expr(positional[0])?.map(|arg| Expr::Aggregate {
                    function: aggregate,
                    arg: Box::new(arg),
                    na_rm,
                }))
            }
            _ => Ok(None),
        }
    }

    /// Whether `expression` reads a column or one of data.table's special symbols anywhere,
    /// which decides whether it runs inside the query or once in the caller's scope.
    unsafe fn mentions_columns(&self, expression: Sexp) -> bool {
        self.mentions(expression, true)
    }

    /// Whether `expression` names a column, or, with `specials`, one of data.table's special
    /// symbols such as `.N`.
    unsafe fn mentions(&self, expression: Sexp, specials: bool) -> bool {
        match expression.kind() {
            SexpType::Symbol => {
                let name = symbol_name(expression);
                self.is_column(&name) || (specials && is_special_symbol(&name))
            }
            SexpType::Language => {
                let mut cell = expression.cdr();
                let head = expression.car();
                if head.kind() == SexpType::Language && self.mentions(head, specials) {
                    return true;
                }
                if head.kind() == SexpType::Symbol && matches!(symbol_name(head).as_ref(), "$" | "@") {
                    return self.mentions(expression.cdr().car(), specials);
                }
                while cell.kind() == SexpType::Pairlist {
                    if self.mentions(cell.car(), specials) {
                        return true;
                    }
                    cell = cell.cdr();
                }
                false
            }
            _ => false,
        }
    }
}

fn is_special_symbol(name: &str) -> bool {
    matches!(name, ".N" | ".SD" | ".I" | ".GRP" | ".BY" | ".NGRP" | ".SDcols")
}

unsafe fn is_call_to(expression: Sexp, name: &str) -> bool {
    expression.kind() == SexpType::Language
        && expression.car().kind() == SexpType::Symbol
        && symbol_name(expression.car()) == name
}

fn aggregate_named(name: &str) -> Option<Aggregate> {
    Some(match name {
        "sum" => Aggregate::Sum,
        "mean" => Aggregate::Mean,
        "min" => Aggregate::Min,
        "max" => Aggregate::Max,
        "first" => Aggregate::First,
        "last" => Aggregate::Last,
        "median" => Aggregate::Median,
        "sd" => Aggregate::Sd,
        "var" => Aggregate::Var,
        "length" => Aggregate::Length,
        _ => return None,
    })
}

fn binary_op(name: &str) -> Option<BinaryOp> {
    Some(match name {
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
        _ => return None,
    })
}

/// A single R value as a literal of the IR, or `None` for anything longer or of another type.
unsafe fn literal(value: Sexp) -> Option<Expr> {
    if length(value) != 1 {
        return None;
    }
    Some(match value.kind() {
        SexpType::Logical => match values::ints(value)[0] {
            NA_INTEGER => Expr::Na(ColumnType::Logical),
            flag => Expr::Logical(flag != 0),
        },
        SexpType::Integer if values::inherits(value, "factor") => {
            let code = values::ints(value)[0];
            let levels = values::get_attr(value, symbols().levels);
            match usize::try_from(code).ok().and_then(|code| code.checked_sub(1)) {
                Some(code) if code < length(levels) => {
                    Expr::Character(char_str(levels.element(code)).into_owned())
                }
                _ => Expr::Na(ColumnType::Character),
            }
        }
        SexpType::Integer => match values::ints(value)[0] {
            NA_INTEGER => Expr::Na(ColumnType::Integer),
            number => Expr::Integer(number),
        },
        SexpType::Real => {
            let number = values::reals(value)[0];
            if is_na_real(number) { Expr::Na(ColumnType::Double) } else { Expr::Double(number) }
        }
        SexpType::String => {
            let element = value.element(0);
            if element == na_string() {
                Expr::Na(ColumnType::Character)
            } else {
                Expr::Character(char_str(element).into_owned())
            }
        }
        _ => return None,
    })
}

unsafe fn roll(value: Sexp, call: Sexp) -> R<Roll> {
    match value.kind() {
        SexpType::Logical => Ok(if values::as_logical_scalar(value) == 1 { Roll::Backward(None) } else { Roll::None }),
        SexpType::Integer | SexpType::Real => {
            let limit = values::as_real_scalar(value);
            Ok(if limit == 0.0 {
                Roll::None
            } else if limit == f64::INFINITY {
                Roll::Backward(None)
            } else if limit == f64::NEG_INFINITY {
                Roll::Forward(None)
            } else if limit > 0.0 {
                Roll::Backward(Some(limit))
            } else {
                Roll::Forward(Some(-limit))
            })
        }
        SexpType::String if values::first_string(value).as_deref() == Some("nearest") => Ok(Roll::Nearest),
        _ => Err(error_in(call, "`roll =` takes TRUE, a number, or \"nearest\"")),
    }
}

// The R fallback.

/// Runs the parts of a query the IR cannot express as R code, in a data mask over the frame.
#[allow(clippy::too_many_arguments)]
unsafe fn fallback(
    subject: Sexp,
    table: &mut Table,
    parts: &Parts,
    rows: Option<Rows>,
    select: Option<Select>,
    by: Option<Vec<(PlSmallStr, Expr)>>,
    lowering: &Lowering<'_>,
) -> R<Sexp> {
    let (call, env) = (lowering.call, lowering.env);
    let by = match by {
        Some(by) if by.iter().all(|(_, expr)| matches!(expr, Expr::Column(_))) => by,
        _ => return Err(error_in(call, "a computed group in `by` that ry cannot run as a query is not supported yet")),
    };
    let positions: Option<Vec<u32>> = match rows {
        Some(Rows::All) => None,
        Some(Rows::Positions(positions)) => Some(positions),
        // R code in `j` would need `i`'s columns in its data mask too, and rows of `i` without
        // a match have no position in `x`.
        Some(Rows::Join(_)) => {
            return Err(error_in(call, "a join whose `j` ry cannot run as a query is not supported yet"));
        }
        Some(rows) => {
            // The IR selects the rows, and the positions follow from a row number column.
            Some(selected_positions(table, rows, call)?)
        }
        None => {
            let Some(piece) = &parts.i else {
                return Err(error_in(call, "the row selection could not be evaluated"));
            };
            let Some(expression) = piece.expression() else {
                return Err(error_in(call, "the row selection could not be evaluated"));
            };
            let mask = data_mask(table, None, expression, env, &[], call)?;
            let value = eval::eval(expression, mask)?;
            Some(row_positions(value, table.height(), call)?)
        }
    };
    let Some(piece) = &parts.j else {
        return subset_rows(table, positions.unwrap_or_default(), call);
    };
    let expression = piece.expression().ok_or_else(|| error_in(call, "`j` could not be evaluated"))?;
    if is_call_to(expression, ":=") {
        if !by.is_empty() {
            return Err(error_in(call, "`:=` with `by` on an expression ry cannot run as a query is not supported yet"));
        }
        let assignments = fallback_assignments(expression, env, call)?;
        let height = table.height();
        let rows_table = match &positions {
            Some(positions) => selected(table, positions.clone(), call)?,
            None => table.fork(),
        };
        let mut columns = Vec::with_capacity(assignments.len());
        for (name, value) in assignments {
            let value = match value {
                None => None,
                Some(value) => {
                    let mask = data_mask(&rows_table, None, value, env, &[], call)?;
                    let computed = eval::eval(value, mask)?;
                    heap().root(computed);
                    Some(match &positions {
                        None => to_column(&name, computed, height, call)?,
                        Some(positions) => scatter(table, &name, positions, computed, call)?,
                    })
                }
            };
            columns.push((name, value));
        }
        let result = assign(subject, table, call, env, |table| {
            for (name, column) in columns {
                match column {
                    Some(column) => table.set_column(column),
                    None => table.drop_column(&name),
                }
                .map_err(|failure| table_error(call, failure))?;
            }
            Ok(())
        })?;
        return Ok(result);
    }
    let rows_table = match &positions {
        Some(positions) => selected(table, positions.clone(), call)?,
        None => table.fork(),
    };
    if select.is_some() && by.is_empty() {
        // Only `i` needed R; the IR runs `j` on the selected rows.
        let mut rows_table = rows_table;
        let select = select.unwrap_or_default();
        return finish(&mut rows_table, Query { select, ..Query::default() }, call);
    }
    let group_names: Vec<PlSmallStr> = by.iter().map(|(name, _)| name.clone()).collect();
    let sd_columns = lowering.sd_columns(parts, &by)?;
    if group_names.is_empty() {
        let mask = data_mask(&rows_table, None, expression, env, &sd_columns, call)?;
        let value = eval::eval(expression, mask)?;
        if value.kind() == SexpType::List && !values::is_nil(value) {
            return wrap(table_from_list(value, call)?);
        }
        return Ok(value);
    }
    grouped_fallback(&rows_table, &group_names, &sd_columns, parts.keyed, expression, env, call)
}

/// The zero-based positions of the rows a lowered row selection keeps.
unsafe fn selected_positions(table: &mut Table, rows: Rows, call: Sexp) -> R<Vec<u32>> {
    let mut numbered = table.fork();
    let height = numbered.height() as i32;
    let index: Vec<i32> = (0..height).collect();
    numbered
        .set_column(convert::to_series(ROW_INDEX.into(), RVector::Integer(index)).map_err(|failure| table_error(call, failure))?.into_column())
        .map_err(|failure| table_error(call, failure))?;
    let query = Query {
        rows,
        select: Select::Value(Expr::Column(ROW_INDEX.into())),
        ..Query::default()
    };
    match query::execute(&mut numbered, query).map_err(|failure| table_error(call, failure))? {
        Output::Vector(series) => Ok(series
            .i32()
            .map_err(|failure| error(failure.to_string()))?
            .into_no_null_iter()
            .map(|position| position as u32)
            .collect()),
        _ => Err(error_in(call, "the row selection did not produce positions")),
    }
}

const ROW_INDEX: &str = ".ry_row";

unsafe fn selected(table: &mut Table, positions: Vec<u32>, call: Sexp) -> R<Table> {
    let query = Query {
        rows: Rows::Positions(positions),
        ..Query::default()
    };
    match query::execute(table, query).map_err(|failure| table_error(call, failure))? {
        Output::Table(result) => Ok(result),
        _ => Err(error_in(call, "the row selection did not produce a frame")),
    }
}

/// The column `name` with `value` written at `positions`, recycled, as `x[i, name := value]`
/// writes it. A new column holds `NA` everywhere else.
unsafe fn scatter(table: &Table, name: &str, positions: &[u32], value: Sexp, call: Sexp) -> R<Column> {
    let height = table.height();
    let base = match table.frame().column(name) {
        Ok(column) => values::duplicate(column_value(column)?)?,
        Err(_) => {
            // `value[rep(NA, height)]` has the value's type and class, all `NA`.
            let index = values::alloc(SexpType::Integer, height)?;
            values::ints_mut(index).fill(NA_INTEGER);
            call_function("[", &[value, index])?
        }
    };
    heap().root(base);
    let index = values::alloc(SexpType::Integer, positions.len())?;
    for (slot, position) in values::ints_mut(index).iter_mut().zip(positions) {
        *slot = *position as i32 + 1;
    }
    let updated = call_function("[<-", &[base, index, value])?;
    to_column(name, updated, height, call)
}

unsafe fn call_function(name: &str, arguments: &[Sexp]) -> R<Sexp> {
    let function = eval::find_fun(install(name)?, values::base_env())?;
    let args: Vec<Arg> = arguments.iter().map(|&value| Arg { tag: nil(), value }).collect();
    eval::apply(nil(), function, &args, values::base_env())
}

/// The `name := value` pairs of a `:=` that runs as R code, with unevaluated values.
unsafe fn fallback_assignments(expression: Sexp, env: Sexp, call: Sexp) -> R<Vec<(String, Option<Sexp>)>> {
    let items = values::pairlist_items(expression.cdr());
    let some = |value: Sexp| if is_nil(value) { None } else { Some(value) };
    if items.iter().all(|(tag, _)| !is_nil(*tag)) {
        return Ok(items.into_iter().map(|(tag, value)| (symbol_name(tag).into_owned(), some(value))).collect());
    }
    if items.len() != 2 {
        return Err(error_in(call, "`:=` takes `name := value` or named arguments"));
    }
    let (left, right) = (items[0].1, items[1].1);
    let names: Vec<String> = match left.kind() {
        SexpType::Symbol => vec![symbol_name(left).into_owned()],
        _ => values::strings(eval::eval(left, env)?).into_iter().map(Option::unwrap_or_default).collect(),
    };
    if names.len() == 1 {
        return Ok(vec![(names[0].clone(), some(right))]);
    }
    if !(is_call_to(right, "list") || is_call_to(right, ".")) {
        return Err(error_in(call, "`:=` with several names takes a `list()` of values"));
    }
    let values = values::pairlist_items(right.cdr());
    if values.len() != names.len() {
        return Err(error_in(call, format!("`:=` names {} columns but gives {} values", names.len(), values.len())));
    }
    Ok(names.into_iter().zip(values.into_iter().map(|(_, value)| some(value))).collect())
}

/// An environment over `env` that binds the columns `expression` mentions, data.table's special
/// symbols, and `..name` variables.
unsafe fn data_mask(
    table: &Table,
    group: Option<(usize, &[u32])>,
    expression: Sexp,
    env: Sexp,
    sd_columns: &[PlSmallStr],
    call: Sexp,
) -> R<Sexp> {
    let mut names = FxHashSet::default();
    collect_symbols(expression, &mut names);
    let mask = values::new_env(env)?;
    // data.table's `.()` is `list()` inside a query.
    let list = eval::find_fun(install("list")?, values::base_env())?;
    eval::define(install(".")?, list, mask)?;
    for name in &names {
        if let Ok(column) = table.frame().column(name) {
            eval::define(install(name)?, column_value(column)?, mask)?;
            continue;
        }
        match name.as_str() {
            ".N" => eval::define(install(name)?, scalar_integer(r_count(table.height())?)?, mask)?,
            ".SD" => {
                let select = Select::Columns(sd_columns.iter().map(|name| (name.clone(), Expr::Column(name.clone()))).collect());
                let mut copy = table.fork();
                let sd = finish(&mut copy, Query { select, ..Query::default() }, call)?;
                eval::define(install(name)?, sd, mask)?;
            }
            ".I" => {
                let rows: Vec<i32> = match group {
                    Some((_, rows)) => rows.iter().map(|row| *row as i32 + 1).collect(),
                    None => (1..=r_count(table.height())?).collect(),
                };
                eval::define(install(name)?, values::integer_vector(&rows)?, mask)?;
            }
            ".GRP" => {
                let number = group.map_or(1, |(number, _)| number + 1);
                eval::define(install(name)?, scalar_integer(r_count(number)?)?, mask)?;
            }
            _ => {
                if let Some(rest) = name.strip_prefix("..")
                    && !rest.is_empty()
                    && !rest.starts_with('.')
                {
                    let value = eval::eval(install(rest)?, env)?;
                    eval::define(install(name)?, value, mask)?;
                }
            }
        }
    }
    Ok(mask)
}

unsafe fn collect_symbols(expression: Sexp, names: &mut FxHashSet<String>) {
    match expression.kind() {
        SexpType::Symbol => {
            names.insert(symbol_name(expression).into_owned());
        }
        SexpType::Language => {
            let mut cell = expression;
            while cell.kind() == SexpType::Language || cell.kind() == SexpType::Pairlist {
                collect_symbols(cell.car(), names);
                cell = cell.cdr();
            }
        }
        _ => {}
    }
}

/// `j` evaluated as R code once per group, with the results stacked under their groups' keys.
unsafe fn grouped_fallback(
    table: &Table,
    group_names: &[PlSmallStr],
    sd_columns: &[PlSmallStr],
    keyed: bool,
    expression: Sexp,
    env: Sexp,
    call: Sexp,
) -> R<Sexp> {
    let fail = |failure: &dyn std::fmt::Display| error_in(call, failure.to_string());
    let mut grouping = table
        .frame()
        .clone()
        .lazy()
        .with_row_index(ROW_INDEX, None)
        .group_by_stable(group_names.iter().map(|name| col(name.clone())).collect::<Vec<_>>())
        .agg([col(ROW_INDEX)]);
    if keyed {
        grouping = grouping.sort(group_names.iter().cloned().collect::<Vec<_>>(), Default::default());
    }
    let groups = grouping.collect().map_err(|failure| fail(&failure))?;
    let rows_column = groups.column(ROW_INDEX).map_err(|failure| fail(&failure))?;
    let rows_list = rows_column.list().map_err(|failure| fail(&failure))?;
    let mut keys_repeat: Vec<u32> = Vec::new();
    let mut names: Vec<String> = Vec::new();
    let mut pieces: Vec<Vec<Sexp>> = Vec::new();
    for group_number in 0..rows_list.len() {
        let Some(rows) = rows_list.get_as_series(group_number) else { continue };
        let rows: Vec<u32> = rows
            .idx()
            .map_err(|failure| fail(&failure))?
            .into_no_null_iter()
            .collect();
        let mut subset = table.fork();
        let group_table = selected(&mut subset, rows.clone(), call)?;
        let value = scope(|| {
            let mask = data_mask(&group_table, Some((group_number, &rows)), expression, env, sd_columns, call)?;
            eval::eval(expression, mask)
        })?;
        if is_nil(value) {
            continue;
        }
        let items: Vec<(String, Sexp)> = if value.kind() == SexpType::List {
            let item_names = values::names(value);
            (0..length(value))
                .map(|index| {
                    let name = if is_nil(item_names) {
                        None
                    } else {
                        values::string_at(item_names, index).filter(|name| !name.is_empty()).map(|name| name.into_owned())
                    };
                    (name.unwrap_or_else(|| format!("V{}", index + 1)), value.element(index))
                })
                .collect()
        } else {
            vec![("V1".to_string(), value)]
        };
        if names.is_empty() {
            names = items.iter().map(|(name, _)| name.clone()).collect();
            pieces = vec![Vec::new(); names.len()];
        } else if items.len() != names.len() {
            return Err(error_in(call, "every group's `j` must produce the same columns"));
        }
        let count = items.iter().map(|(_, item)| length(*item)).max().unwrap_or(0);
        for (slot, (_, item)) in pieces.iter_mut().zip(&items) {
            let item = if length(*item) == count {
                *item
            } else {
                call_function("rep_len", &[*item, scalar_integer(r_count(count)?)?])?
            };
            heap().root(item);
            slot.push(item);
        }
        keys_repeat.extend(std::iter::repeat_n(group_number as u32, count));
    }
    let keys = groups
        .select(group_names.iter().cloned())
        .map_err(|failure| fail(&failure))?;
    let mut keys_table = Table::new(keys).map_err(|failure| table_error(call, failure))?;
    let mut result = selected(&mut keys_table, keys_repeat, call)?;
    for (name, items) in names.iter().zip(pieces) {
        let combined = crate::subset::combine(&items)?;
        heap().root(combined);
        let column = to_column(name, combined, result.height(), call)?;
        result.set_column(column).map_err(|failure| table_error(call, failure))?;
    }
    wrap(result)
}
