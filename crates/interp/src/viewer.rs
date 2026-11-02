//! The data viewer's side in the interpreter: an editor opens a view of a data frame, a ry
//! `DataFrame`, a matrix or a vector, and then pages through its rows. A view pins its object
//! until the editor releases it or newer views push it out, so a page can be fetched after the
//! variable that held the object was reassigned.
//!
//! Only the rows of the requested page are ever formatted, which keeps a view of a million-row
//! table as cheap to open as one of ten rows. A page may ask for the rows filtered by a search
//! text and sorted by a column. The view then keeps the matching row numbers in order, never a
//! sorted copy of the data, and reuses them while later pages ask for the same order. A ry
//! `DataFrame` filters and sorts in its table engine; an R object through R's `grepl()` and
//! `order()`. An R object's page is formatted by R's own
//! `format()` on that slice, so classes such as factors and dates read as they print; a ry
//! `DataFrame` page is a slice of its columns.
//!
//! Everything here runs on the interpreter's thread. The debugger answers these requests while
//! the program is stopped, and the console server while the console waits for input.

use {
    crate::{
        coerce, eval, frame, interp, lower,
        unwind::{Jump, R, Unwind},
        values::{
            self, base_env, class_attr, get_attr, global_env, length, nil, scalar_integer,
            symbol_name, symbols,
        },
    },
    heap::object::{Sexp, SexpType, gp},
    serde_json::{Value, json},
    std::{cell::RefCell, collections::VecDeque},
    table::polars::prelude::AnyValue,
};

/// The views the editor holds open. The oldest is released once this many are open, because an
/// editor that never releases its views must not pin every object it ever showed.
const MAX_VIEWS: usize = 32;

/// The most rows one page may ask for.
const MAX_PAGE: usize = 1000;

thread_local! {
    static VIEWS: RefCell<Views> = const { RefCell::new(Views { next: 1, open: VecDeque::new() }) };
}

struct Views {
    next: u64,
    open: VecDeque<View>,
}

struct View {
    id: u64,
    object: Sexp,
    shape: Shape,
    order: Option<Order>,
}

/// Drops every view's cached order, so the next page sorts and filters the object as it is now.
pub fn forget_orders() {
    VIEWS.with(|views| {
        for view in &mut views.borrow_mut().open {
            view.order = None;
        }
    });
}

/// A view's rows as one search and sort leave them: the zero-based row numbers, in the order
/// they are shown.
struct Order {
    search: String,
    sort: Option<(usize, bool)>,
    rows: Vec<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Shape {
    /// A ry `DataFrame`.
    Table,
    /// An R data frame: each element is a column.
    DataFrame,
    Matrix,
    /// Any other vector or list, shown as one column.
    Vector,
}

/// Opens a view of `object` and describes it: its id, its size and its columns. Fails for a value
/// that has no tabular shape, such as a function.
pub unsafe fn open(object: Sexp, title: &str) -> Result<Value, String> {
    let shape = shape_of(object).ok_or_else(|| {
        format!(
            "a value of type {} cannot be shown as a table",
            coerce::type_name(object)
        )
    })?;
    let (rows, columns) = describe(object, shape).map_err(message)?;
    crate::heap().preserve(object);
    let id = VIEWS.with(|views| {
        let mut views = views.borrow_mut();
        let id = views.next;
        views.next += 1;
        views.open.push_back(View {
            id,
            object,
            shape,
            order: None,
        });
        if views.open.len() > MAX_VIEWS
            && let Some(oldest) = views.open.pop_front()
        {
            crate::heap().release(oldest.object);
        }
        id
    });
    Ok(json!({
        "viewId": id,
        "title": title,
        "rows": rows,
        "columns": columns,
        "class": class_name(object),
    }))
}

/// Answers `ryTable` (a page of rows) and `ryRelease` (closes a view). A `ryTable` request may
/// carry `filter`, a text that some cell of a shown row contains (ignoring case), and `sort`,
/// `{ "column": <zero-based index>, "descending": <bool> }`; its `total` is the number of rows
/// they leave.
pub unsafe fn request(command: &str, arguments: &Value) -> Result<Value, String> {
    let id = arguments
        .get("viewId")
        .and_then(Value::as_u64)
        .ok_or("the request needs a `viewId`")?;
    match command {
        "ryRelease" => {
            let released = VIEWS.with(|views| {
                let mut views = views.borrow_mut();
                let index = views.open.iter().position(|view| view.id == id)?;
                views.open.remove(index)
            });
            if let Some(view) = released {
                crate::heap().release(view.object);
            }
            Ok(json!({}))
        }
        "ryTable" => {
            let start = arguments.get("start").and_then(Value::as_u64).unwrap_or(0) as usize;
            let count = arguments
                .get("count")
                .and_then(Value::as_u64)
                .unwrap_or(100)
                .min(MAX_PAGE as u64) as usize;
            let search = arguments
                .get("filter")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_lowercase();
            let sort = arguments.get("sort").and_then(|sort| {
                let column = usize::try_from(sort.get("column")?.as_u64()?).ok()?;
                Some((
                    column,
                    sort.get("descending").and_then(Value::as_bool) == Some(true),
                ))
            });
            // The order is taken out of the view while R code runs, because a user's `format`
            // method may open another view.
            let (object, shape, cached) = VIEWS
                .with(|views| {
                    let mut views = views.borrow_mut();
                    let view = views.open.iter_mut().find(|view| view.id == id)?;
                    Some((view.object, view.shape, view.order.take()))
                })
                .ok_or("the view was closed")?;
            let mark = crate::heap().handle_mark();
            let order = match (search.is_empty() && sort.is_none(), cached) {
                (true, _) => Ok(None),
                (false, Some(order)) if order.search == search && order.sort == sort => {
                    Ok(Some(order))
                }
                (false, _) => rows_in_order(object, shape, &search, sort).map(|rows| {
                    Some(Order {
                        search: search.clone(),
                        sort,
                        rows,
                    })
                }),
            };
            let page = order.and_then(|order| {
                let rows = order.as_ref().map(|order| order.rows.as_slice());
                let page = page(object, shape, rows, start, count).map_err(message);
                VIEWS.with(|views| {
                    let mut views = views.borrow_mut();
                    if let Some(view) = views.open.iter_mut().find(|view| view.id == id) {
                        view.order = order;
                    }
                });
                page
            });
            crate::heap().release_handles(mark);
            page
        }
        command => Err(format!("`{command}` is not a data viewer request")),
    }
}

/// The global environment's bindings, for an editor's completion and hover. Promises are not
/// forced, active bindings are not called, and shapes are read from attributes, so listing them
/// runs no R code: a hover must never dispatch to a user's `dim` method or hit a `browser()`.
pub unsafe fn globals() -> Value {
    let env = global_env();
    let mut bindings = Vec::new();
    for symbol in crate::envir::env_symbols(env, false, true) {
        let Some(cell) = crate::heap().frame_cell(env, symbol) else {
            continue;
        };
        let name = symbol_name(symbol).into_owned();
        if cell.has_gp(gp::ACTIVE_BINDING) {
            bindings.push(json!({ "name": name, "type": "active binding" }));
            continue;
        }
        let mut value = cell.cdr();
        if value.kind() == SexpType::Promise {
            if value.car() == values::unbound() {
                bindings.push(json!({ "name": name, "type": "promise" }));
                continue;
            }
            value = value.car();
        }
        let mut binding = json!({
            "name": name,
            "type": coerce::type_name(value),
            "class": class_name(value),
            "summary": crate::debugger::summary(value),
        });
        if let Some(shape) = shape_of(value)
            && shape != Shape::Vector
            && let Ok((_, columns)) = describe(value, shape)
        {
            binding["columns"] = columns;
        }
        bindings.push(binding);
    }
    json!({ "globals": bindings })
}

unsafe fn shape_of(object: Sexp) -> Option<Shape> {
    if frame::is_frame(object) {
        return Some(Shape::Table);
    }
    let kind = object.kind();
    if !(kind.is_atomic() || kind == SexpType::List) {
        return None;
    }
    if values::inherits(object, "data.frame") {
        return Some(Shape::DataFrame);
    }
    if values::dims(object).is_some_and(|dims| dims.len() == 2) {
        return Some(Shape::Matrix);
    }
    Some(Shape::Vector)
}

/// The row count and the columns, each with its name and type.
unsafe fn describe(object: Sexp, shape: Shape) -> R<(usize, Value)> {
    let column = |name: String, kind: String| json!({ "name": name, "type": kind });
    Ok(match shape {
        Shape::Table => {
            let table = frame::table_of(object)?;
            let columns = table
                .frame()
                .columns()
                .iter()
                .map(|series| column(series.name().to_string(), series.dtype().to_string()))
                .collect();
            (table.height(), Value::Array(columns))
        }
        Shape::DataFrame => {
            let row_names = get_attr(object, symbols().row_names);
            let rows = match crate::stats_models::compact_row_count(row_names) {
                Some(rows) => rows,
                None if row_names == nil() => match length(object) {
                    0 => 0,
                    _ => length(object.element(0)),
                },
                None => length(row_names),
            };
            let names = values::names(object);
            let columns = (0..length(object))
                .map(|index| {
                    let name =
                        element_name(names, index).unwrap_or_else(|| format!("V{}", index + 1));
                    column(name, class_name(object.element(index)))
                })
                .collect();
            (rows, Value::Array(columns))
        }
        Shape::Matrix => {
            let dims = values::dims(object).unwrap_or_default();
            let (rows, count) = (
                dims.first().copied().unwrap_or(0),
                dims.get(1).copied().unwrap_or(0),
            );
            let dimnames = get_attr(object, symbols().dimnames);
            let names = match dimnames.kind() == SexpType::List && length(dimnames) == 2 {
                true => dimnames.element(1),
                false => nil(),
            };
            let kind = coerce::type_name(object).to_owned();
            let columns = (0..count)
                .map(|index| {
                    let name =
                        element_name(names, index).unwrap_or_else(|| format!("[,{}]", index + 1));
                    column(name, kind.clone())
                })
                .collect();
            (rows, Value::Array(columns))
        }
        Shape::Vector => (
            length(object),
            json!([column("value".to_owned(), class_name(object))]),
        ),
    })
}

/// The zero-based numbers of the rows that contain `search` (already lower case) in some cell,
/// in the order `sort` (a column index and whether it sorts descending) gives them.
unsafe fn rows_in_order(
    object: Sexp,
    shape: Shape,
    search: &str,
    sort: Option<(usize, bool)>,
) -> Result<Vec<u32>, String> {
    let (rows, columns) = describe(object, shape).map_err(message)?;
    let width = columns.as_array().map_or(0, Vec::len);
    if let Some((column, _)) = sort
        && column >= width
    {
        return Err(format!("the view has no column {}", column + 1));
    }
    if shape == Shape::Table {
        let table = frame::table_of(object).map_err(message)?;
        let names: Vec<String> = table
            .frame()
            .columns()
            .iter()
            .map(|series| series.name().to_string())
            .collect();
        let sort =
            sort.and_then(|(column, descending)| Some((names.get(column)?.as_str(), descending)));
        return table::ops::view_rows(table, search, sort).map_err(|failure| failure.to_string());
    }
    let column = match shape {
        Shape::DataFrame => "x[[k]]",
        Shape::Matrix => "x[, k]",
        _ => "x",
    };
    let template = format!(
        "{{
            column <- function(k) {column}
            rows <- seq_len(n)
            if (nzchar(f)) {{
                keep <- logical(n)
                for (k in seq_len(w))
                    keep <- keep | (!is.na(column(k)) &
                        grepl(f, tolower(format(column(k), trim = TRUE)), fixed = TRUE))
                rows <- rows[keep]
            }}
            if (s > 0) rows <- rows[order(column(s)[rows], decreasing = d, na.last = FALSE)]
            rows
        }}"
    );
    let (sorted, descending) =
        sort.map_or((0, false), |(column, descending)| (column + 1, descending));
    let bindings = [
        ("n", scalar_integer(rows as i32).map_err(message)?),
        ("w", scalar_integer(width as i32).map_err(message)?),
        ("f", values::scalar_string(search).map_err(message)?),
        ("s", scalar_integer(sorted as i32).map_err(message)?),
        (
            "d",
            values::scalar_logical(i32::from(descending)).map_err(message)?,
        ),
    ];
    let rows = run(&template, object, &bindings).map_err(message)?;
    if rows.kind() != SexpType::Integer {
        return Err("sorting the view did not give row numbers".to_owned());
    }
    Ok(rows
        .slice::<i32>()
        .iter()
        .filter_map(|&row| u32::try_from(row - 1).ok())
        .collect())
}

/// Rows `start .. start + count` as strings, row-major, with the row names when the object has
/// them. With `order`, the rows are counted in that order, and a row is named by its number in
/// the object when the object names none.
unsafe fn page(
    object: Sexp,
    shape: Shape,
    order: Option<&[u32]>,
    start: usize,
    count: usize,
) -> R<Value> {
    let (rows, columns) = describe(object, shape)?;
    let total = order.map_or(rows, <[u32]>::len);
    let end = total.min(start.saturating_add(count));
    let start = start.min(end);
    let width = columns.as_array().map_or(0, Vec::len);
    let mut cells = vec![vec![String::new(); width]; end - start];
    let mut row_names = None;
    let shown = order.map(|order| &order[start..end]);
    if shape == Shape::Table {
        let table = frame::table_of(object)?;
        let slice = match shown {
            Some(shown) => table::ops::take_rows(table, shown)
                .map_err(|failure| crate::unwind::error(failure.to_string()))?,
            None => table.frame().slice(start as i64, end - start),
        };
        for (column, series) in slice.columns().iter().enumerate() {
            for (row, line) in cells.iter_mut().enumerate() {
                line[column] = match series.get(row) {
                    Ok(AnyValue::Null) => "NA".to_owned(),
                    Ok(AnyValue::String(text)) => text.to_owned(),
                    Ok(AnyValue::StringOwned(text)) => text.to_string(),
                    Ok(value) => value.to_string(),
                    Err(failure) => failure.to_string(),
                };
            }
        }
    } else if end > start {
        let numbers: Vec<i32> = match shown {
            Some(shown) => shown.iter().map(|&row| row as i32 + 1).collect(),
            None => (start + 1..=end).map(|index| index as i32).collect(),
        };
        let indices = values::integer_vector(&numbers)?;
        let template = match shape {
            Shape::DataFrame => "format(x[[j]][i], trim = TRUE)",
            Shape::Matrix => "format(x[i, j], trim = TRUE)",
            _ => "format(x[i], trim = TRUE)",
        };
        for column in 0..width {
            let bindings = [("i", indices), ("j", scalar_integer(column as i32 + 1)?)];
            let formatted = run(template, object, &bindings).and_then(|value| {
                if value.kind() == SexpType::String {
                    Ok(value)
                } else {
                    coerce::as_vector(value, "character")
                }
            });
            for (row, line) in cells.iter_mut().enumerate() {
                line[column] = match &formatted {
                    Ok(strings) if row < length(*strings) => string_at(*strings, row),
                    Ok(_) => String::new(),
                    Err(failure) => format!("<{}>", message_of(failure)),
                };
            }
        }
        let template = match shape {
            Shape::DataFrame => "as.character(row.names(x)[i])",
            Shape::Matrix => "as.character(rownames(x)[i])",
            _ => "as.character(names(x)[i])",
        };
        let names = run(template, object, &[("i", indices)])?;
        if names.kind() == SexpType::String && length(names) == end - start {
            row_names = Some(
                (0..end - start)
                    .map(|row| string_at(names, row))
                    .collect::<Vec<_>>(),
            );
        }
    }
    if row_names.is_none()
        && let Some(shown) = shown
    {
        row_names = Some(shown.iter().map(|row| (row + 1).to_string()).collect());
    }
    Ok(json!({ "start": start, "total": total, "rows": cells, "rowNames": row_names }))
}

/// Evaluates a fixed R expression with `x` bound to the object and each of `bindings` to its
/// value. It is evaluated in a fresh environment over base, so the user's bindings cannot shadow
/// `format` or `order`, while S3 methods still dispatch.
unsafe fn run(template: &str, object: Sexp, bindings: &[(&str, Sexp)]) -> R<Sexp> {
    let env = values::new_env(base_env())?;
    eval::define(values::install("x")?, object, env)?;
    for &(name, value) in bindings {
        eval::define(values::install(name)?, value, env)?;
    }
    let expressions = lower::parse_text(template, "<data viewer>")?;
    crate::heap().root(expressions);
    let state = interp();
    let shown = std::mem::replace(&mut state.show_error_messages, false);
    let srcref = state.srcref;
    let result = eval::eval(expressions.element(0), env);
    let state = interp();
    state.show_error_messages = shown;
    state.srcref = srcref;
    let value = result?;
    crate::heap().root(value);
    Ok(value)
}

fn message(failure: Unwind) -> String {
    match failure {
        Unwind::Error(failure) => failure.message,
        Unwind::Interrupt => "interrupted".to_owned(),
        Unwind::Jump(jump) if matches!(*jump, Jump::TopLevel) => {
            interp().error_message.trim_end().to_owned()
        }
        Unwind::Jump(_) => "the data viewer's code jumped".to_owned(),
    }
}

fn message_of(failure: &Unwind) -> String {
    match failure {
        Unwind::Error(failure) => failure.message.clone(),
        _ => "the value could not be formatted".to_owned(),
    }
}

unsafe fn class_name(object: Sexp) -> String {
    let classes = class_attr(object);
    if classes.kind() == SexpType::String && length(classes) > 0 {
        return string_at(classes, 0);
    }
    coerce::type_name(object).to_owned()
}

unsafe fn element_name(names: Sexp, index: usize) -> Option<String> {
    if names == nil() || names.kind() != SexpType::String || index >= length(names) {
        return None;
    }
    let name = string_at(names, index);
    (!name.is_empty()).then_some(name)
}

unsafe fn string_at(strings: Sexp, index: usize) -> String {
    let element = values::string_elt(strings, index);
    if element == values::na_string() {
        return "NA".to_owned();
    }
    values::char_str(element).into_owned()
}
