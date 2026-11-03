//! Printing values the way GNU R's `print.default` does, and auto-printing at top level.

use {
    crate::{
        builtins::{BuiltinDef, Call, internal},
        deparse,
        eval::{self, value_of},
        format::{self, Settings, display_width, encode_string, pad},
        interp,
        unwind::R,
        values::{self, char_str, get_attr, length, nil, symbol_name, symbols},
        write_console,
    },
    heap::object::{Sexp, SexpType, gp},
};

pub const BUILTINS: &[BuiltinDef] = &[
    internal("print.default", 3, do_print_default).invisible(),
    internal("print.function", 3, do_print_function).invisible(),
    internal("prmatrix", 6, |call| {
        Err(call.error("prmatrix is not supported"))
    }),
];

/// Print settings, from `print.default`'s arguments and the options.
#[derive(Clone)]
pub struct PrintOptions {
    pub settings: Settings,
    pub quote: bool,
    pub right: bool,
    pub gap: usize,
    pub width: usize,
    pub na_string: String,
    pub na_string_noquote: String,
    pub max: usize,
    pub use_source: bool,
    /// The environment `print` methods are looked up from.
    pub env: Sexp,
    /// Extra arguments passed through to print methods for list elements.
    pub call_args: Vec<(Sexp, Sexp)>,
}

impl PrintOptions {
    pub unsafe fn defaults(env: Sexp) -> PrintOptions {
        let max = crate::envir::get_option("max.print");
        PrintOptions {
            settings: Settings::current(),
            quote: true,
            right: false,
            gap: 1,
            width: format::width_option(),
            na_string: "NA".into(),
            na_string_noquote: "<NA>".into(),
            max: if max == nil() {
                99999
            } else {
                values::as_integer_scalar(max).max(1) as usize
            },
            use_source: true,
            env,
            call_args: Vec::new(),
        }
    }
}

fn out(text: &str) {
    write_console(text.as_bytes(), false);
}

/// Prints a value at top level: objects and functions go through `print` (or `show` for S4),
/// everything else through `print.default`'s machinery.
pub unsafe fn print_value_top(value: Sexp, env: Sexp) -> R<()> {
    // A ry value has an implicit class, so it prints through `print` and a user's
    // `print.ry_record` method, once base R has defined `print`.
    let dispatches = value.kind() == SexpType::Record && {
        let print = values::install("print")?;
        eval::find_var(print, values::base_namespace())? != values::unbound()
    };
    if value.is_object() || values::is_function(value) || dispatches {
        let function = if value.has_gp(gp::S4_OBJECT) && interp().globals.methods_namespace != nil()
        {
            let show = values::install("show")?;
            eval::find_fun(show, interp().globals.methods_namespace)?
        } else {
            let print = values::install("print")?;
            eval::find_fun(print, values::base_namespace())?
        };
        let x = values::install("x")?;
        let call = values::call(function, &[x])?;
        let print_env = values::new_env(env)?;
        crate::eval::define(x, value, print_env)?;
        eval::eval(call, print_env)?;
        return Ok(());
    }
    let options = PrintOptions::defaults(env);
    print_value(value, &options, &mut String::new())
}

/// Prints a value with dispatch, as `print.default` does for list elements that are objects.
unsafe fn print_dispatch(value: Sexp, options: &PrintOptions, tag: &mut String) -> R<()> {
    if value.is_object() {
        let function = if value.has_gp(gp::S4_OBJECT) && interp().globals.methods_namespace != nil()
        {
            eval::find_fun(values::install("show")?, interp().globals.methods_namespace)?
        } else {
            eval::find_fun(values::install("print")?, values::base_namespace())?
        };
        let x = values::install("x")?;
        let env = values::new_env(options.env)?;
        crate::eval::define(x, value, env)?;
        let mut items = vec![(nil(), function), (nil(), x)];
        if !value.has_gp(gp::S4_OBJECT) {
            items.extend(options.call_args.iter().copied());
        }
        let call = values::pairlist(&items, SexpType::Language)?;
        eval::eval(call, env)?;
        return Ok(());
    }
    print_value(value, options, tag)
}

/// `PrintValueRec`: prints any value, with `tag` the prefix of the list element being printed.
pub unsafe fn print_value(value: Sexp, options: &PrintOptions, tag: &mut String) -> R<()> {
    match value.kind() {
        _ if value.is_array() => {
            for line in deparse::deparse_lines(value, 60, deparse::options::SIMPLE) {
                out(&format!("{line}\n"));
            }
        }
        SexpType::Nil => out("NULL\n"),
        SexpType::Symbol if value == values::missing_arg() => out("\n"),
        SexpType::Symbol => {
            let name = symbol_name(value);
            out(&format!("{}\n", deparse::quote_name(&name)));
        }
        SexpType::Special | SexpType::Builtin => {
            print_primitive(value)?;
        }
        SexpType::Closure => print_closure(value, options)?,
        SexpType::Environment => out(&format!("{}\n", environment_label(value)?)),
        SexpType::Promise => out("<promise>\n"),
        SexpType::Dots => out("<...>\n"),
        SexpType::Language => {
            for line in deparse::deparse_lines(value, 60, deparse::options::SIMPLE) {
                out(&format!("{line}\n"));
            }
        }
        SexpType::Expression => {
            let opts = match options.use_source {
                true => deparse::options::SIMPLE | deparse::options::USE_SOURCE,
                false => deparse::options::SIMPLE,
            };
            let lines = deparse::deparse_lines(value, 60, opts);
            for line in lines {
                out(&format!("{line}\n"));
            }
        }
        SexpType::Pairlist => {
            let list = values::pairlist_to_list(value)?;
            print_list(list, options, tag)?;
        }
        SexpType::List => {
            if values::dims(value).is_some_and(|dims| dims.len() == 2) {
                print_list_matrix(value, options)?;
            } else {
                print_list(value, options, tag)?;
                return Ok(());
            }
        }
        SexpType::Record => {
            for line in deparse::deparse_lines(value, 60, deparse::options::SIMPLE) {
                out(&format!("{line}\n"));
            }
        }
        SexpType::ExternalPtr => out(&format!("{}\n", pointer_text(value))),
        SexpType::WeakRef => out("<weak reference>\n"),
        SexpType::Bytecode => out("<bytecode>\n"),
        SexpType::Object => {
            if value.has_gp(gp::S4_OBJECT) {
                out("<S4 Type Object>\n");
            } else {
                out("<object>\n");
            }
        }
        SexpType::Char => out(&format!(
            "<CHARSXP: {}>\n",
            encode_string(&char_str(value), Some('"'))
        )),
        kind if kind.is_atomic() => {
            let dims = values::dims(value);
            match dims.as_deref() {
                Some([rows, cols]) => print_matrix(value, *rows, *cols, options, true)?,
                Some(dims) if dims.len() > 2 => print_array(value, dims, options)?,
                Some([_]) => {
                    // A one-dimensional array with a named dimnames shows the name, even an
                    // empty one, on a line of its own, as a 1-D table does.
                    let dimnames_names = values::names(get_attr(value, symbols().dimnames));
                    if dimnames_names != nil() {
                        let title = values::string_at(dimnames_names, 0)
                            .map(|text| text.into_owned())
                            .unwrap_or_default();
                        out(&format!("{title}\n"));
                    }
                    print_vector(value, options)?
                }
                _ => print_vector(value, options)?,
            }
        }
        _ => out("<unknown>\n"),
    }
    print_attributes(value, options, tag)
}

unsafe fn print_primitive(value: Sexp) -> R<()> {
    let name = values::primitive_name(value);
    // Primitives print their formals from `.ArgsEnv` when there are any.
    let mut header = String::from("function ");
    let mut found = false;
    for env_name in [".ArgsEnv", ".GenericArgsEnv"] {
        let symbol = values::install(env_name)?;
        let env = eval::find_var_in_frame(symbol, values::base_namespace())?;
        if env == values::unbound() {
            continue;
        }
        let env = value_of(env)?;
        if env.kind() != SexpType::Environment {
            continue;
        }
        let function = eval::find_var_in_frame(values::install(name)?, env)?;
        if function != values::unbound() {
            let function = value_of(function)?;
            if function.kind() == SexpType::Closure {
                let lines = deparse::deparse_lines(
                    values::triple(
                        SexpType::Closure,
                        function.car(),
                        nil(),
                        values::global_env(),
                    )?,
                    60,
                    deparse::options::SIMPLE,
                );
                header = lines.first().cloned().unwrap_or_default();
                header = header.trim_end().to_owned();
                header.push(' ');
                found = true;
                break;
            }
        }
    }
    if !found {
        header = "function () ".to_owned();
    }
    out(&format!("{header} .Primitive(\"{name}\")\n"));
    Ok(())
}

unsafe fn print_closure(function: Sexp, options: &PrintOptions) -> R<()> {
    let source = match options.use_source {
        true => deparse::source_lines(get_attr(function, symbols().srcref)),
        false => None,
    };
    let lines =
        source.unwrap_or_else(|| deparse::deparse_lines(function, 60, deparse::options::SIMPLE));
    for line in lines {
        out(&format!("{line}\n"));
    }
    let env = function.tag();
    if env != values::global_env() {
        if crate::bytecode::is_compiled(function) {
            out("<bytecode>\n");
        }
        out(&format!("{}\n", environment_label(env)?));
    }
    Ok(())
}

/// How `print` shows an environment.
pub unsafe fn environment_label(env: Sexp) -> R<String> {
    if env == values::global_env() {
        return Ok("<environment: R_GlobalEnv>".into());
    }
    if env == values::base_env() {
        return Ok("<environment: base>".into());
    }
    if env == values::empty_env() {
        return Ok("<environment: R_EmptyEnv>".into());
    }
    if crate::envir::is_namespace(env)
        && let Some(name) = crate::envir::namespace_name(env)
    {
        return Ok(format!("<environment: namespace:{name}>"));
    }
    if crate::envir::is_package_env(env) {
        let name = get_attr(env, symbols().name);
        if let Some(name) = values::first_string(name) {
            return Ok(format!("<environment: {name}>"));
        }
    }
    Ok(format!("<environment: {:p}>", env.as_ptr()))
}

/// The `[i]` label width for a vector of `count` elements.
fn label_width(count: usize) -> usize {
    count.max(1).to_string().len() + 2
}

pub unsafe fn print_vector(vector: Sexp, options: &PrintOptions) -> R<()> {
    let count = length(vector);
    if count == 0 {
        let empty = match vector.kind() {
            SexpType::Logical => "logical(0)",
            SexpType::Integer => "integer(0)",
            SexpType::Real => "numeric(0)",
            SexpType::Complex => "complex(0)",
            SexpType::String => "character(0)",
            SexpType::Raw => "raw(0)",
            _ => "NULL",
        };
        let named = values::get_attr(vector, symbols().names) != nil();
        out(&format!("{}{empty}\n", if named { "named " } else { "" }));
        return Ok(());
    }
    let shown = count.min(options.max);
    let names = values::names(vector);
    let na = if vector.kind() == SexpType::String && !options.quote {
        &options.na_string_noquote
    } else {
        &options.na_string
    };
    let subset = if shown < count {
        let indices: Vec<usize> = (0..shown).collect();
        crate::subset::select_elements(vector, &indices)?
    } else {
        vector
    };
    let items = escape_unquoted(
        format::format_vector(subset, options.settings, options.quote, na)?,
        vector,
        options,
    );
    let measures = measure_items(subset, &items, options.quote);
    if names != nil() {
        print_named_vector(
            &items,
            &measures,
            names,
            vector.kind() == SexpType::Complex,
            options,
        );
    } else {
        let right = vector.kind() != SexpType::String || options.right;
        let width = measures
            .iter()
            .map(|measure| measure.column)
            .max()
            .unwrap_or(0);
        let labels = label_width(shown);
        let mut line = String::new();
        let mut used = 0;
        for (index, item) in items.iter().enumerate() {
            if index == 0 || used + width + options.gap > options.width {
                if index > 0 {
                    line.push('\n');
                }
                let label = format!("[{}]", index + 1);
                line.push_str(&pad(&label, labels, true));
                used = labels;
            }
            line.push_str(&" ".repeat(options.gap));
            line.push_str(&pad_measured(item, measures[index], width, right));
            used += width + options.gap;
        }
        line.push('\n');
        out(&line);
    }
    if shown < count {
        out(&format!(
            "{} {} entries ]\n",
            max_reached_prefix(),
            count - shown
        ));
    }
    Ok(())
}

/// The start of the note that `max.print` cut a printout short. R 4.5 also names the `max`
/// argument of `print()`, which sets the same limit.
fn max_reached_prefix() -> &'static str {
    match crate::system::r_at_least(4, 5) {
        true => " [ reached 'max' / getOption(\"max.print\") -- omitted",
        false => " [ reached getOption(\"max.print\") -- omitted",
    }
}

/// `count` and its noun, singular for one: GNU R's `ngettext(" %d row", " %d rows", count)`.
fn counted(count: usize, singular: &str, plural: &str) -> String {
    match count {
        1 => format!(" 1 {singular}"),
        _ => format!(" {count} {plural}"),
    }
}

unsafe fn print_named_vector(
    items: &[String],
    measures: &[Measure],
    names: Sexp,
    complex: bool,
    options: &PrintOptions,
) {
    let name_texts: Vec<String> = (0..items.len())
        .map(|index| match values::string_at(names, index) {
            Some(text) => text.into_owned(),
            None => "<NA>".to_owned(),
        })
        .collect();
    let width = measures
        .iter()
        .map(|measure| measure.column)
        .chain(name_texts.iter().map(|name| display_width(name)))
        .max()
        .unwrap_or(0);
    let per_line = (options.width / (width + options.gap)).max(1);
    let mut text = String::new();
    for chunk_start in (0..items.len()).step_by(per_line) {
        let chunk_end = (chunk_start + per_line).min(items.len());
        for name in &name_texts[chunk_start..chunk_end] {
            text.push_str(&pad(name, width, true));
            text.push_str(&" ".repeat(options.gap));
        }
        text.push('\n');
        if complex {
            // GNU R prints named complex values at their own width, unpadded to the names and
            // without a trailing gap.
            text.push_str(&items[chunk_start..chunk_end].join(&" ".repeat(options.gap)));
        } else {
            for index in chunk_start..chunk_end {
                text.push_str(&pad_measured(&items[index], measures[index], width, true));
                text.push_str(&" ".repeat(options.gap));
            }
        }
        text.push('\n');
    }
    out(&text);
}

/// The text of each column of a matrix, formatted per column as GNU R does.
unsafe fn matrix_columns(
    matrix: Sexp,
    rows: usize,
    shown: usize,
    cols: usize,
    options: &PrintOptions,
) -> R<Vec<Vec<(String, Measure)>>> {
    let na = if matrix.kind() == SexpType::String && !options.quote {
        &options.na_string_noquote
    } else {
        &options.na_string
    };
    let mut columns = Vec::with_capacity(cols);
    for col in 0..cols {
        // The whole column sets the common width and digits, as in GNU R, even when
        // `max.print` hides some of its rows.
        let indices: Vec<usize> = (0..rows).map(|row| row + col * rows).collect();
        let column = crate::subset::select_elements(matrix, &indices)?;
        let mut items = format::format_vector(column, options.settings, options.quote, na)?;
        items.truncate(shown);
        let items = escape_unquoted(items, matrix, options);
        let measures = measure_items(column, &items, options.quote);
        columns.push(items.into_iter().zip(measures).collect());
    }
    Ok(columns)
}

/// Unquoted strings still print with their control characters and backslashes escaped.
unsafe fn escape_unquoted(items: Vec<String>, vector: Sexp, options: &PrintOptions) -> Vec<String> {
    if vector.kind() != SexpType::String || options.quote {
        return items;
    }
    items
        .into_iter()
        .map(|item| encode_string(&item, None))
        .collect()
}

/// The columns GNU R counts for a printed element: the width its column must have, and the
/// width its padding assumes it takes. Both are the display width, except for a "bytes" string.
/// GNU R measures one by its `\xNN` spelling before backslashes are escaped, so it prints wider
/// than it counts, and its padding counts an escaped quote as two columns where its column
/// width counts one.
#[derive(Clone, Copy)]
struct Measure {
    column: usize,
    counted: usize,
}

unsafe fn measure_items(vector: Sexp, items: &[String], quote: bool) -> Vec<Measure> {
    items
        .iter()
        .enumerate()
        .map(|(index, item)| {
            let element = match vector.kind() {
                SexpType::String => Some(vector.element(index)),
                _ => None,
            };
            match element.filter(|element| element.has_gp(gp::BYTES)) {
                Some(element) => {
                    let bytes = element.bytes();
                    let spelling: usize = bytes
                        .iter()
                        .map(|&byte| if (0x20..0x80).contains(&byte) { 1 } else { 4 })
                        .sum();
                    let quotes = if quote { 2 } else { 0 };
                    let escaped_quotes = match quote {
                        true => bytes.iter().filter(|&&byte| byte == b'"').count(),
                        false => 0,
                    };
                    Measure {
                        column: spelling + quotes,
                        counted: spelling + quotes + escaped_quotes,
                    }
                }
                None => {
                    let width = display_width(item);
                    Measure {
                        column: width,
                        counted: width,
                    }
                }
            }
        })
        .collect()
}

/// Pads `text` to `width` columns as though it took `measure.counted` of them.
fn pad_measured(text: &str, measure: Measure, width: usize, left: bool) -> String {
    let padding = " ".repeat(width.saturating_sub(measure.counted));
    if left {
        padding + text
    } else {
        text.to_owned() + &padding
    }
}

pub unsafe fn print_matrix(
    matrix: Sexp,
    rows: usize,
    cols: usize,
    options: &PrintOptions,
    top: bool,
) -> R<()> {
    if !top {
        return print_matrix_rows(matrix, rows, cols, rows, options);
    }
    // GNU R prints whole rows only, as many as fit within `max` entries. R 4.5 also stops at
    // `max` columns, and then prints at least one row.
    let truncates_columns = crate::system::r_at_least(4, 5);
    let shown_cols = match truncates_columns {
        true => cols.min(options.max),
        false => cols,
    };
    let mut shown = if cols > 0 && options.max / cols < rows {
        options.max / cols
    } else {
        rows
    };
    if cols > shown_cols && shown < 1 && rows > 0 {
        shown = 1;
    }
    print_matrix_rows(matrix, rows, shown_cols, shown, options)?;
    if truncates_columns && (shown < rows || shown_cols < cols) {
        let mut note = max_reached_prefix().to_owned();
        if shown < rows {
            note.push_str(&counted(rows - shown, "row", "rows"));
        }
        if shown_cols < cols {
            if shown < rows {
                note.push_str(" and");
            }
            note.push_str(&counted(cols - shown_cols, "column", "columns"));
        }
        note.push_str(" ]\n");
        out(&note);
    } else if shown < rows {
        out(&format!(
            "{}{} ]\n",
            max_reached_prefix(),
            counted(rows - shown, "row", "rows")
        ));
    }
    Ok(())
}

/// Prints the first `shown` rows of a matrix. Row labels keep the width of all `rows`.
unsafe fn print_matrix_rows(
    matrix: Sexp,
    rows: usize,
    cols: usize,
    shown: usize,
    options: &PrintOptions,
) -> R<()> {
    if rows == 0 && cols == 0 {
        out("<0 x 0 matrix>\n");
        return Ok(());
    }
    let dimnames = get_attr(matrix, symbols().dimnames);
    let (row_names, col_names) = if dimnames.kind() == SexpType::List && length(dimnames) == 2 {
        (dimnames.element(0), dimnames.element(1))
    } else {
        (nil(), nil())
    };
    let dimnames_names = values::names(dimnames);
    let row_title = if dimnames_names != nil() {
        values::string_at(dimnames_names, 0)
            .map(|text| text.into_owned())
            .unwrap_or_default()
    } else {
        String::new()
    };
    let col_title = if dimnames_names != nil() && length(dimnames_names) > 1 {
        values::string_at(dimnames_names, 1)
            .map(|text| text.into_owned())
            .unwrap_or_default()
    } else {
        String::new()
    };
    let row_labels: Vec<String> = (0..rows)
        .map(|row| {
            if row_names != nil() {
                values::string_at(row_names, row).map_or_else(
                    || options.na_string_noquote.clone(),
                    |text| text.into_owned(),
                )
            } else {
                format!("[{},]", row + 1)
            }
        })
        .collect();
    // Row names align left, but GNU R right-aligns the `[i,]` labels that stand in for them.
    // Named dimnames (even named "") indent the row labels by at least two columns, or enough
    // for the row title, and always print the column title line.
    let numbered_rows = row_names == nil();
    let titled = dimnames_names != nil();
    // GNU R sizes numbered labels by the digits of one past the last row, so nine rows already
    // reserve two digits and no rows reserve one. A matrix without rows reserves that width
    // even when it has (empty) row names.
    let numbered_width = if numbered_rows || rows == 0 {
        format!("[{},]", rows + 1).len()
    } else {
        0
    };
    let label_width = row_labels
        .iter()
        .map(|label| display_width(label))
        .max()
        .unwrap_or(0)
        .max(numbered_width);
    let offset = if !titled {
        0
    } else if display_width(&row_title) < label_width + 2 {
        2
    } else {
        display_width(&row_title) - label_width
    };
    let row_label_width = label_width + offset;
    let row_labels: Vec<String> = row_labels
        .into_iter()
        .map(|label| {
            if numbered_rows {
                pad(&label, row_label_width, true)
            } else {
                pad(
                    &format!("{}{label}", " ".repeat(offset)),
                    row_label_width,
                    false,
                )
            }
        })
        .collect();
    let columns = matrix_columns(matrix, rows, shown, cols, options)?;
    let is_string = matrix.kind() == SexpType::String;
    let right = !is_string || options.right;
    let col_labels: Vec<String> = (0..cols)
        .map(|col| {
            if col_names != nil() {
                values::string_at(col_names, col).map_or_else(
                    || options.na_string_noquote.clone(),
                    |text| text.into_owned(),
                )
            } else {
                format!("[,{}]", col + 1)
            }
        })
        .collect();
    let widths: Vec<usize> = (0..cols)
        .map(|col| {
            columns[col]
                .iter()
                .map(|(_, measure)| measure.column)
                .max()
                .unwrap_or(0)
                .max(display_width(&col_labels[col]))
        })
        .collect();
    if cols == 0 {
        // GNU R prints the row labels alone, under the column title line of named dimnames.
        let mut text = String::new();
        if titled {
            text.push_str(&" ".repeat(row_label_width));
            text.push_str(&col_title);
            text.push('\n');
        }
        text.push_str(&pad(&row_title, row_label_width, false));
        text.push('\n');
        for label in &row_labels[..shown] {
            text.push_str(label);
            text.push('\n');
        }
        out(&text);
        return Ok(());
    }
    let mut start = 0;
    while start < cols {
        let mut end = start;
        let mut used = row_label_width;
        // GNU R starts a new block once a line would reach the full width, not only exceed it.
        while end < cols && (end == start || used + widths[end] + options.gap < options.width) {
            used += widths[end] + options.gap;
            end += 1;
        }
        let mut text = String::new();
        if titled {
            text.push_str(&" ".repeat(row_label_width));
            text.push_str(&col_title);
            text.push('\n');
        }
        text.push_str(&pad(&row_title, row_label_width, false));
        // Column labels, named or `[,j]`, align like the column's entries, except that before
        // R 4.6 a right-aligned character matrix left-aligns its `[,j]` labels.
        let labels_right =
            right && (col_names != nil() || !is_string || crate::system::r_at_least(4, 6));
        for col in start..end {
            text.push_str(&" ".repeat(options.gap));
            text.push_str(&pad(&col_labels[col], widths[col], labels_right));
        }
        text.push('\n');
        for row in 0..shown {
            text.push_str(&row_labels[row]);
            for col in start..end {
                text.push_str(&" ".repeat(options.gap));
                let (item, measure) = &columns[col][row];
                text.push_str(&pad_measured(item, *measure, widths[col], right));
            }
            text.push('\n');
        }
        out(&text);
        start = end;
    }
    Ok(())
}

unsafe fn print_array(array: Sexp, dims: &[usize], options: &PrintOptions) -> R<()> {
    let rows = dims[0];
    let cols = dims[1];
    let slice = rows * cols;
    let slices: usize = dims[2..].iter().product();
    let dimnames = get_attr(array, symbols().dimnames);
    let dimnames_names = if dimnames.kind() == SexpType::List {
        values::names(dimnames)
    } else {
        nil()
    };
    // GNU R prints whole slices up to `max` entries, then the whole rows of a last slice that
    // fit. R 4.5 fills that last slice up to `max` entries: one partial row when fewer than a
    // row's entries remain.
    let truncates_columns = crate::system::r_at_least(4, 5);
    let max_reached = slice > 0 && options.max < slice * slices;
    let (shown_slices, last_rows, last_cols) = if max_reached && truncates_columns {
        let shown = options.max.div_ceil(slice);
        let remaining = options.max - slice * (shown - 1);
        match remaining < cols {
            true => (shown, 1, remaining),
            false => (shown, remaining / cols, cols),
        }
    } else if max_reached && cols > 0 {
        let shown = options.max.div_ceil(slice);
        let last_rows = (options.max - slice * (shown - 1)) / cols;
        if last_rows == 0 {
            (shown - 1, rows, cols)
        } else {
            (shown, last_rows, cols)
        }
    } else {
        (slices, rows, cols)
    };
    for index in 0..shown_slices {
        let mut label = String::from(", , ");
        let mut remainder = index;
        let mut parts = Vec::new();
        for (dimension, &extent) in dims.iter().enumerate().skip(2) {
            let position = remainder % extent;
            remainder /= extent;
            let names = if dimnames.kind() == SexpType::List {
                dimnames.element(dimension)
            } else {
                nil()
            };
            if names != nil() {
                let name = values::string_at(names, position)
                    .map(|text| text.into_owned())
                    .unwrap_or_default();
                // Named dimnames label each slice `dimension = name`, even when the dimension's
                // own name is empty.
                if dimnames_names == nil() {
                    parts.push(name);
                } else {
                    let title = values::string_at(dimnames_names, dimension).unwrap_or_default();
                    parts.push(format!("{title} = {name}"));
                }
            } else {
                parts.push((position + 1).to_string());
            }
        }
        label.push_str(&parts.join(", "));
        out(&format!("{label}\n\n"));
        let indices: Vec<usize> = (index * slice..(index + 1) * slice).collect();
        let matrix = crate::subset::select_elements(array, &indices)?;
        let dim = values::integer_vector(&[rows as i32, cols as i32])?;
        values::set_attr(matrix, symbols().dim, dim)?;
        if dimnames.kind() == SexpType::List {
            let sub = values::list_vector(&[dimnames.element(0), dimnames.element(1)])?;
            if dimnames_names != nil() {
                let titles = values::string_vector(&[
                    &values::string_at(dimnames_names, 0).unwrap_or_default(),
                    &values::string_at(dimnames_names, 1).unwrap_or_default(),
                ])?;
                values::set_attr(sub, symbols().names, titles)?;
            }
            values::set_attr(matrix, symbols().dimnames, sub)?;
        }
        let (shown, shown_cols) = if index + 1 == shown_slices {
            (last_rows, last_cols)
        } else {
            (rows, cols)
        };
        print_matrix_rows(matrix, rows, shown_cols, shown, options)?;
        out("\n");
    }
    if max_reached && truncates_columns {
        let mut note = max_reached_prefix().to_owned();
        if shown_slices < slices {
            note.push_str(&counted(slices - shown_slices, "slice", "slices"));
        } else {
            if rows > last_rows {
                note.push_str(&counted(rows - last_rows, "row", "rows"));
            }
            if cols > last_cols {
                note.push_str(&counted(cols - last_cols, "column", "columns"));
            }
        }
        note.push_str(" ] \n");
        out(&note);
    } else if shown_slices < slices {
        let omitted_rows = if last_rows < rows {
            format!(" {} row(s) and", rows - last_rows)
        } else {
            String::new()
        };
        out(&format!(
            "{}{omitted_rows} {} matrix slice(s) ]\n",
            max_reached_prefix(),
            slices - shown_slices
        ));
    }
    Ok(())
}

unsafe fn print_list_matrix(list: Sexp, options: &PrintOptions) -> R<()> {
    // A list with dims prints each cell's summary, as GNU R does.
    let dims = values::dims(list).unwrap_or_default();
    let count = length(list);
    let texts = values::alloc(SexpType::String, count)?;
    for index in 0..count {
        let element = list.element(index);
        let text = summarize_cell(element)?;
        values::set_elt(texts, index, values::mkchar(&text)?);
    }
    values::copy_most_attributes(list, texts, true)?;
    let mut options = options.clone();
    options.quote = false;
    print_matrix(texts, dims[0], dims[1], &options, true)
}

unsafe fn summarize_cell(element: Sexp) -> R<String> {
    Ok(match element.kind() {
        SexpType::Nil => "NULL".into(),
        kind if kind.is_atomic() && length(element) == 1 => {
            let items = format::format_vector(element, Settings::current(), true, "NA")?;
            items.into_iter().next().unwrap_or_default()
        }
        SexpType::Logical => format!("logical,{}", length(element)),
        SexpType::Integer => format!("integer,{}", length(element)),
        SexpType::Real => format!("numeric,{}", length(element)),
        SexpType::String => format!("character,{}", length(element)),
        SexpType::List => format!("list,{}", length(element)),
        SexpType::Closure => "?".into(),
        _ => "?".into(),
    })
}

unsafe fn print_list(list: Sexp, options: &PrintOptions, tag: &mut String) -> R<()> {
    let count = length(list);
    if count == 0 {
        let names = values::names(list);
        if names != nil() {
            out("named list()\n");
        } else {
            out("list()\n");
        }
        return print_attributes(list, options, tag);
    }
    let names = values::names(list);
    let saved = tag.len();
    // GNU R prints the whole list when it is at most one element over `max`.
    let shown = if count <= options.max.saturating_add(1) {
        count
    } else {
        options.max
    };
    for index in 0..shown {
        let element = list.element(index);
        let name = if names != nil() {
            values::string_at(names, index).map(|text| text.into_owned())
        } else {
            None
        };
        // GNU R's tag holds 256 bytes: a name that would overflow it becomes `$...`, and
        // nothing more is added once it is past that length.
        const TAG_LIMIT: usize = 256;
        if saved <= TAG_LIMIT {
            match name {
                Some(name) if !name.is_empty() => {
                    let piece = if deparse::is_syntactic(&name) {
                        format!("${name}")
                    } else {
                        format!("$`{name}`")
                    };
                    if saved + piece.len() <= TAG_LIMIT {
                        tag.push_str(&piece);
                    } else {
                        tag.push_str("$...");
                    }
                }
                Some(_) | None => tag.push_str(&format!("[[{}]]", index + 1)),
            }
        }
        out(&format!("{tag}\n"));
        if element.is_object() {
            print_dispatch(element, options, tag)?;
        } else {
            print_value(element, options, tag)?;
        }
        tag.truncate(saved);
        out("\n");
    }
    if shown < count {
        out(&format!(
            "{} {} entries ]\n",
            max_reached_prefix(),
            count - shown
        ));
    }
    print_attributes(list, options, tag)
}

/// Prints the attributes `print.default` shows after a value.
unsafe fn print_attributes(value: Sexp, options: &PrintOptions, tag: &mut String) -> R<()> {
    let symbols = symbols();
    let mut cell = value.attrib();
    while cell.kind() == SexpType::Pairlist {
        let name = cell.tag();
        let skip = name == symbols.names
            || name == symbols.dim
            || name == symbols.dimnames
            || name == symbols.srcref
            || name == symbols.whole_srcref
            || name == symbols.srcfile
            || (name == symbols.comment)
            || (name == symbols.row_names && values::inherits(value, "data.frame"))
            || value.kind() == SexpType::Pairlist;
        if !skip {
            let saved = tag.clone();
            // An attribute's label continues the labels of the attributes it is nested in, but
            // not the list element tags around them.
            if !tag.starts_with("attr(") {
                tag.clear();
            }
            let label = format!("attr(,\"{}\")", symbol_name(name));
            tag.push_str(&label);
            out(&format!("{tag}\n"));
            let attribute = cell.car();
            if name == symbols.row_names {
                let expanded = crate::subset::subset(attribute, &[], false, nil())?;
                print_value(expanded, options, tag)?;
            } else if attribute.is_object() {
                print_dispatch(attribute, options, tag)?;
            } else {
                print_value(attribute, options, tag)?;
            }
            *tag = saved;
        }
        cell = cell.cdr();
    }
    Ok(())
}

/// `print.default(x, args, missings)`.
unsafe fn do_print_default(call: &Call<'_>) -> R<Sexp> {
    let value = call.arg(0);
    let args = call.arg(1);
    let mut options = PrintOptions::defaults(call.env);
    let missings = call.arg(2);
    let mut call_args = Vec::new();
    for (position, (tag, argument)) in values::pairlist_items(args).into_iter().enumerate() {
        let argument = value_of(argument)?;
        // Nested `print()` calls get the arguments the caller supplied, as in GNU R, so a method
        // that does not take `quote` never sees the default.
        let supplied = missings.kind() != SexpType::Logical
            || values::ints(missings)
                .get(position)
                .is_none_or(|missing| *missing == 0);
        if supplied {
            call_args.push((tag, argument));
        }
        let name = if tag == nil() {
            String::new()
        } else {
            symbol_name(tag).into_owned()
        };
        match name.as_str() {
            "digits" => {
                if argument != nil() {
                    let digits = values::as_integer_scalar(argument);
                    if !(1..=22).contains(&digits) {
                        return Err(call.error("invalid digits parameter (1 <= digits <= 22)"));
                    }
                    options.settings.digits = digits as usize;
                }
            }
            "quote" => {
                options.quote = values::as_logical_scalar(argument) != 0;
            }
            "na.print" => {
                if argument != nil() {
                    let text = values::first_string(argument)
                        .map(|text| text.into_owned())
                        .unwrap_or_default();
                    options.na_string = text.clone();
                    options.na_string_noquote = text;
                }
            }
            "print.gap" => {
                if argument != nil() {
                    options.gap = values::as_integer_scalar(argument).max(0) as usize;
                }
            }
            "right" => options.right = values::as_logical_scalar(argument) == 1,
            "max" => {
                if argument != nil() {
                    options.max = values::as_integer_scalar(argument).max(1) as usize;
                }
            }
            "width" => {
                if argument != nil() {
                    options.width = values::as_integer_scalar(argument).max(10) as usize;
                }
            }
            "useSource" => options.use_source = values::as_logical_scalar(argument) != 0,
            _ => {}
        }
    }
    options.call_args = call_args;
    if value.has_gp(gp::S4_OBJECT)
        && value.is_object()
        && interp().globals.methods_namespace != nil()
    {
        print_dispatch(value, &options, &mut String::new())?;
    } else {
        print_value(value, &options, &mut String::new())?;
    }
    Ok(value)
}

/// `print.function(x, useSource, ...)`.
unsafe fn do_print_function(call: &Call<'_>) -> R<Sexp> {
    let function = call.arg(0);
    let mut options = PrintOptions::defaults(call.env);
    options.use_source = values::as_logical_scalar(call.arg(1)) != 0;
    match function.kind() {
        SexpType::Closure => print_closure(function, &options)?,
        SexpType::Builtin | SexpType::Special => print_primitive(function)?,
        _ => print_value(function, &options, &mut String::new())?,
    }
    Ok(function)
}

/// An external pointer as GNU R prints it, through C's `%p`: glibc writes a null pointer as
/// `(nil)`.
pub unsafe fn pointer_text(object: Sexp) -> String {
    let address = object.external_address();
    if address.is_null() {
        "<pointer: (nil)>".to_owned()
    } else {
        format!("<pointer: {address:p}>")
    }
}
