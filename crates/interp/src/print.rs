//! Printing values the way GNU R's `print.default` does, and auto-printing at top level.

use crate::{
    builtins::{BuiltinDef, Call, internal},
    deparse,
    eval::{self, value_of},
    format::{self, Settings, display_width, encode_string, pad},
    interp,
    unwind::R,
    values::{self, char_str, get_attr, length, nil, symbol_name, symbols},
    write_console,
};
use heap::object::{Sexp, SexpType, gp};

pub const BUILTINS: &[BuiltinDef] = &[
    internal("print.default", 3, do_print_default).invisible(),
    internal("print.function", 3, do_print_function).invisible(),
    internal("prmatrix", 6, |call| unsafe { Err(call.error("prmatrix is not supported")) }),
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
            max: if max == nil() { 99999 } else { values::as_integer_scalar(max).max(1) as usize },
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
    if value.is_object() || values::is_function(value) {
        let function = if value.has_gp(gp::S4_OBJECT) && interp().globals.methods_namespace != nil() {
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
        let function = if value.has_gp(gp::S4_OBJECT) && interp().globals.methods_namespace != nil() {
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
        SexpType::Nil => out("NULL\n"),
        SexpType::Symbol if value == values::missing_arg() => out("\n"),
        SexpType::Symbol => {
            let name = symbol_name(value);
            out(&format!("{}\n", deparse::quote_name(&name)));
        }
        SexpType::Special | SexpType::Builtin => {
            print_primitive(value)?;
        }
        SexpType::Closure => print_closure(value)?,
        SexpType::Environment => out(&format!("{}\n", environment_label(value)?)),
        SexpType::Promise => out("<promise>\n"),
        SexpType::Language => {
            for line in deparse::deparse_lines(value, 60, deparse::options::SIMPLE) {
                out(&format!("{line}\n"));
            }
        }
        SexpType::Expression => {
            let lines = deparse::deparse_lines(value, 60, deparse::options::SIMPLE);
            for line in lines {
                out(&format!("{line}\n"));
            }
        }
        SexpType::Pairlist | SexpType::Dots => {
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
        SexpType::ExternalPtr => out("<pointer: 0x0>\n"),
        SexpType::WeakRef => out("<weak reference>\n"),
        SexpType::Bytecode => out("<bytecode>\n"),
        SexpType::Object => {
            if value.has_gp(gp::S4_OBJECT) {
                out("<S4 Type Object>\n");
            } else {
                out("<object>\n");
            }
        }
        SexpType::Char => out(&format!("<CHARSXP: {}>\n", encode_string(&char_str(value), Some('"')))),
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
                        let title = values::string_at(dimnames_names, 0).map(|text| text.into_owned()).unwrap_or_default();
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
                    values::triple(SexpType::Closure, function.car(), nil(), values::global_env())?,
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

unsafe fn print_closure(function: Sexp) -> R<()> {
    let lines = deparse::deparse_lines(function, 60, deparse::options::SIMPLE);
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
    if crate::envir::is_namespace(env) {
        if let Some(name) = crate::envir::namespace_name(env) {
            return Ok(format!("<environment: namespace:{name}>"));
        }
    }
    if crate::envir::is_package_env(env) {
        let name = get_attr(env, values::install("name")?);
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
    let items = escape_unquoted(format::format_vector(subset, options.settings, options.quote, na)?, vector, options);
    if names != nil() {
        print_named_vector(&items, names, options);
    } else {
        let right = vector.kind() != SexpType::String || options.right;
        let width = items.iter().map(|item| display_width(item)).max().unwrap_or(0);
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
            line.push_str(&pad(item, width, right));
            used += width + options.gap;
        }
        line.push('\n');
        out(&line);
    }
    if shown < count {
        out(&format!(
            " [ reached getOption(\"max.print\") -- omitted {} entries ]\n",
            count - shown
        ));
    }
    Ok(())
}

unsafe fn print_named_vector(items: &[String], names: Sexp, options: &PrintOptions) {
    let name_texts: Vec<String> = (0..items.len())
        .map(|index| match values::string_at(names, index) {
            Some(text) => text.into_owned(),
            None => "<NA>".to_owned(),
        })
        .collect();
    let width = items
        .iter()
        .map(|item| display_width(item))
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
        for item in &items[chunk_start..chunk_end] {
            text.push_str(&pad(item, width, true));
            text.push_str(&" ".repeat(options.gap));
        }
        text.push('\n');
    }
    out(&text);
}

/// The text of each column of a matrix, formatted per column as GNU R does.
unsafe fn matrix_columns(matrix: Sexp, rows: usize, cols: usize, options: &PrintOptions) -> R<Vec<Vec<String>>> {
    let na = if matrix.kind() == SexpType::String && !options.quote {
        &options.na_string_noquote
    } else {
        &options.na_string
    };
    let mut columns = Vec::with_capacity(cols);
    for col in 0..cols {
        let indices: Vec<usize> = (0..rows).map(|row| row + col * rows).collect();
        let column = crate::subset::select_elements(matrix, &indices)?;
        columns.push(escape_unquoted(format::format_vector(column, options.settings, options.quote, na)?, matrix, options));
    }
    Ok(columns)
}

/// Unquoted strings still print with their control characters and backslashes escaped.
unsafe fn escape_unquoted(items: Vec<String>, vector: Sexp, options: &PrintOptions) -> Vec<String> {
    if vector.kind() != SexpType::String || options.quote {
        return items;
    }
    items.into_iter().map(|item| encode_string(&item, None)).collect()
}

pub unsafe fn print_matrix(
    matrix: Sexp,
    rows: usize,
    cols: usize,
    options: &PrintOptions,
    _top: bool,
) -> R<()> {
    let dimnames = get_attr(matrix, symbols().dimnames);
    let (row_names, col_names) = if dimnames.kind() == SexpType::List && length(dimnames) == 2 {
        (dimnames.element(0), dimnames.element(1))
    } else {
        (nil(), nil())
    };
    let dimnames_names = values::names(dimnames);
    let row_title = if dimnames_names != nil() {
        values::string_at(dimnames_names, 0).map(|text| text.into_owned()).unwrap_or_default()
    } else {
        String::new()
    };
    let col_title = if dimnames_names != nil() && length(dimnames_names) > 1 {
        values::string_at(dimnames_names, 1).map(|text| text.into_owned()).unwrap_or_default()
    } else {
        String::new()
    };
    let row_labels: Vec<String> = (0..rows)
        .map(|row| {
            if row_names != nil() {
                values::string_at(row_names, row).map_or("<NA>".to_owned(), |text| text.into_owned())
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
    let label_width = row_labels.iter().map(|label| display_width(label)).max().unwrap_or(0);
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
                pad(&format!("{}{label}", " ".repeat(offset)), row_label_width, false)
            }
        })
        .collect();
    let columns = matrix_columns(matrix, rows, cols, options)?;
    let is_string = matrix.kind() == SexpType::String;
    let right = !is_string || options.right;
    let col_labels: Vec<String> = (0..cols)
        .map(|col| {
            if col_names != nil() {
                values::string_at(col_names, col).map_or("<NA>".to_owned(), |text| text.into_owned())
            } else {
                format!("[,{}]", col + 1)
            }
        })
        .collect();
    let widths: Vec<usize> = (0..cols)
        .map(|col| {
            columns[col]
                .iter()
                .map(|item| display_width(item))
                .max()
                .unwrap_or(0)
                .max(display_width(&col_labels[col]))
        })
        .collect();
    if cols == 0 {
        // GNU R prints the row labels alone.
        let mut text = String::new();
        text.push_str(&pad(&row_title, row_label_width, false));
        text.push('\n');
        for label in &row_labels {
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
        while end < cols && (end == start || used + widths[end] + options.gap <= options.width) {
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
        for col in start..end {
            text.push_str(&" ".repeat(options.gap));
            // Column labels, named or `[,j]`, align like the column's entries.
            text.push_str(&pad(&col_labels[col], widths[col], right));
        }
        text.push('\n');
        for row in 0..rows {
            text.push_str(&row_labels[row]);
            for col in start..end {
                text.push_str(&" ".repeat(options.gap));
                text.push_str(&pad(&columns[col][row], widths[col], right));
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
    for index in 0..slices {
        let mut label = String::from(", , ");
        let mut remainder = index;
        let mut parts = Vec::new();
        for (dimension, &extent) in dims.iter().enumerate().skip(2) {
            let position = remainder % extent;
            remainder /= extent;
            let names = if dimnames.kind() == SexpType::List { dimnames.element(dimension) } else { nil() };
            if names != nil() {
                parts.push(values::string_at(names, position).map(|text| text.into_owned()).unwrap_or_default());
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
            values::set_attr(matrix, symbols().dimnames, sub)?;
        }
        print_matrix(matrix, rows, cols, options, false)?;
        out("\n");
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
    for index in 0..count {
        let element = list.element(index);
        let name = if names != nil() {
            values::string_at(names, index).map(|text| text.into_owned())
        } else {
            None
        };
        match name {
            Some(name) if !name.is_empty() => {
                if deparse::is_syntactic(&name) {
                    tag.push_str(&format!("${name}"));
                } else {
                    tag.push_str(&format!("$`{name}`"));
                }
            }
            Some(_) | None => tag.push_str(&format!("[[{}]]", index + 1)),
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
    let mut call_args = Vec::new();
    // Arguments the caller left missing are not passed on to the print methods of list
    // elements, as GNU R's `missings` says.
    let missings = call.arg(2);
    let missing = |position: usize| -> bool {
        missings.kind() == SexpType::Logical && position < length(missings) && values::ints(missings)[position] == 1
    };
    for (position, (tag, argument)) in values::pairlist_items(args).into_iter().enumerate() {
        let passed = !missing(position);
        let call_args = &mut call_args;
        let mut call_args_push = |item: (Sexp, Sexp)| {
            if passed {
                call_args.push(item);
            }
        };
        let argument = value_of(argument)?;
        let name = if tag == nil() { String::new() } else { symbol_name(tag).into_owned() };
        match name.as_str() {
            "digits" => {
                if argument != nil() {
                    let digits = values::as_integer_scalar(argument);
                    if !(1..=22).contains(&digits) {
                        return Err(call.error("invalid digits parameter (1 <= digits <= 22)"));
                    }
                    options.settings.digits = digits as usize;
                    call_args_push((tag, argument));
                }
            }
            "quote" => {
                options.quote = values::as_logical_scalar(argument) != 0;
                call_args_push((tag, argument));
            }
            "na.print" => {
                if argument != nil() {
                    let text = values::first_string(argument).map(|text| text.into_owned()).unwrap_or_default();
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
            _ => call_args_push((tag, argument)),
        }
    }
    options.call_args = call_args;
    if value.has_gp(gp::S4_OBJECT) && value.is_object() && interp().globals.methods_namespace != nil() {
        print_dispatch(value, &options, &mut String::new())?;
    } else {
        print_value(value, &options, &mut String::new())?;
    }
    Ok(value)
}

unsafe fn do_print_function(call: &Call<'_>) -> R<Sexp> {
    let function = call.arg(0);
    let options = PrintOptions::defaults(call.env);
    match function.kind() {
        SexpType::Closure => print_closure(function)?,
        SexpType::Builtin | SexpType::Special => print_primitive(function)?,
        _ => print_value(function, &options, &mut String::new())?,
    }
    Ok(function)
}
