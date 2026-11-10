# Differential harness for ry's data.table bridge. `seed`, `count` and (optionally) `only` are
# defined above this file. Every query runs twice in this process, once with the bridge on and
# once with options(ry.datatable = FALSE), on freshly built copies of the same tables, and the
# two runs must be identical in value, attributes and their order, the tables afterwards,
# printed output and every condition.
if (!requireNamespace("data.table", quietly = TRUE)) {
  cat("data.table is not installed\n")
  quit(status = 0)
}
suppressPackageStartupMessages(library(data.table))
has_bit64 <- requireNamespace("bit64", quietly = TRUE)
if (has_bit64) suppressPackageStartupMessages(library(bit64))
setDTthreads(1L)

pick <- function(x) x[[sample.int(length(x), 1L)]]
chance <- function(p) runif(1L) < p

make_column <- function(kind, n) {
  switch(kind,
    g = {
      v <- sample(c("a", "b", "c", "ab", "", "Z", "é"), n, TRUE)
      if (chance(0.5)) v[sample.int(n, min(n, 2L))] <- NA
      v
    },
    h = {
      v <- sample(c(1L, 2L, 3L, 5L), n, TRUE)
      if (chance(0.5)) v[sample.int(n, min(n, 2L))] <- NA_integer_
      v
    },
    x = {
      v <- sample(c(0, 1, 2.5, -1, 10, 3), n, TRUE)
      if (chance(0.6)) v[sample.int(n, min(n, 2L))] <- pick(list(NA_real_, NaN, Inf, -Inf))
      v
    },
    y = round(rnorm(n), 2),
    b = {
      v <- sample(c(TRUE, FALSE), n, TRUE)
      if (chance(0.5)) v[sample.int(n, min(n, 1L))] <- NA
      v
    },
    f = {
      v <- factor(sample(c("lo", "mid", "hi"), n, TRUE), levels = pick(list(c("lo", "mid", "hi"), c("hi", "lo", "mid", "unused"))))
      if (chance(0.4)) v[sample.int(n, min(n, 1L))] <- NA
      v
    },
    i = as.integer64(sample(c(1, 2, 3, 100), n, TRUE))
  )
}

kinds <- c("g", "h", "x", "y", "b", "f", if (has_bit64) "i")

# The tables a query sees are described by a plain list so both runs rebuild them identically.
describe_tables <- function() {
  n <- pick(c(0L, 1L, 2L, 5L, 12L, 40L, 40L))
  chosen <- unique(c(pick(c("g", "h")), sample(kinds, sample(2:5, 1L))))
  key <- if (chance(0.15)) pick(chosen) else NULL
  list(
    n = n, columns = chosen, key = key,
    side_n = pick(c(0L, 1L, 3L, 6L)), side_key = chance(0.5),
    seed = sample.int(1e6, 1L)
  )
}

build_tables <- function(spec) {
  set.seed(spec$seed)
  columns <- lapply(spec$columns, make_column, n = spec$n)
  names(columns) <- spec$columns
  dt <- as.data.table(columns)
  if (!is.null(spec$key)) setkeyv(dt, spec$key)
  side <- data.table(
    g = sample(c("a", "b", "c", "zz", NA), spec$side_n, TRUE),
    h = sample(c(1L, 2L, 3L, 9L, NA), spec$side_n, TRUE),
    x = sample(c(0, 2.5, 7, NA), spec$side_n, TRUE),
    w = round(runif(spec$side_n), 2)
  )
  if (spec$side_key && spec$side_n > 0L) setkeyv(side, "g")
  list(dt = dt, o = side)
}

col_of <- function(spec, types = NULL) {
  pool <- spec$columns
  if (!is.null(types)) pool <- intersect(pool, types)
  if (!length(pool)) NULL else pick(pool)
}

literal_for <- function(column) {
  switch(column,
    g = pick(c('"a"', '"b"', '"zz"', 'NA_character_')),
    h = pick(c("1L", "2L", "3L", "NA_integer_", "lim")),
    x = pick(c("0", "2.5", "1", "NA_real_", "NaN", "Inf", "lim")),
    y = pick(c("0", "0.5", "-1")),
    b = pick(c("TRUE", "FALSE", "NA")),
    f = pick(c('"lo"', '"hi"', '"nope"')),
    i = pick(c("1", "100", "as.integer64(2)"))
  )
}

gen_filter <- function(spec) {
  column <- col_of(spec)
  lit <- literal_for(column)
  switch(sample.int(10L, 1L),
    paste0(column, " > ", if (column %in% c("g", "f", "b")) lit else lit),
    paste0(column, " == ", lit),
    paste0(column, " != ", lit),
    paste0("!is.na(", column, ")"),
    paste0("is.na(", column, ")"),
    paste0(column, " %in% c(", lit, ", ", literal_for(column), ")"),
    paste0("order(", pick(c("", "-")), if (column %in% c("g", "f", "b", "i")) column else column, ")"),
    paste0(column, " >= ", lit, " & ", col_of(spec), " != ", literal_for(col_of(spec))),
    paste0("(", column, " == ", lit, ") | ", "!is.na(", col_of(spec), ")"),
    pick(c("1L", "2:3", "-1L", "c(1L, NA)", "0L", "c(TRUE, FALSE)", "99L", "seq_len(.N)", ".N", "rev(seq_len(.N))", "NULL", "integer(0)", "c(2L, 2L)"))
  )
}

gen_join_i <- function(spec) {
  switch(sample.int(7L, 1L),
    paste0("o, on = ", pick(c("\"g\"", "\"h\"", "c(\"g\", \"h\")", ".(g)", ".(h, x)"))),
    paste0("o, on = ", pick(c("\"g\"", "\"h\"")), ", nomatch = 0"),
    paste0("o, on = \"g\", mult = \"", pick(c("first", "last", "all")), "\""),
    paste0("o, on = ", pick(c("\"h\"", ".(h, x)")), ", roll = ", pick(c("TRUE", "-Inf", "Inf", "\"nearest\""))),
    "o, on = .(x >= x)",
    "o, on = \"g\", nomatch = NULL",
    paste0(".(", literal_for(pick(c("g", "h"))), ")")
  )
}

gen_j_scalar <- function(spec) {
  column <- col_of(spec)
  numeric_col <- col_of(spec, c("h", "x", "y"))
  switch(sample.int(8L, 1L),
    paste0("sum(", numeric_col %||% column, ")"),
    paste0("sum(", numeric_col %||% column, ", na.rm = TRUE)"),
    paste0("mean(", numeric_col %||% column, ")"),
    paste0("max(", column, ")"),
    paste0("min(", column, ", na.rm = TRUE)"),
    paste0("length(", column, ")"),
    paste0("uniqueN(", column, ")"),
    ".N"
  )
}

`%||%` <- function(a, b) if (is.null(a)) b else a

gen_j <- function(spec, grouped) {
  column <- col_of(spec)
  other <- col_of(spec)
  numeric_col <- col_of(spec, c("h", "x", "y")) %||% column
  switch(sample.int(22L, 1L),
    column,
    paste0(".(", column, ")"),
    paste0(".(", column, ", ", other, ")"),
    paste0(".(a = ", gen_j_scalar(spec), ")"),
    paste0(".(a = ", gen_j_scalar(spec), ", b = ", gen_j_scalar(spec), ")"),
    gen_j_scalar(spec),
    ".N",
    ".(n = .N)",
    ".(n = .N, s = sum(", numeric_col, ", na.rm = TRUE))",
    paste0(".(z = ", numeric_col, " * 2)"),
    paste0(".(z = ", numeric_col, " + 1L, w = ", column, ")"),
    paste0("c(\"", column, "\", \"", other, "\")"),
    paste0("-\"", column, "\""),
    ".SD",
    ".SD[1L]",
    ".SD[.N]",
    paste0("lapply(.SD, function(v) sum(!is.na(v)))"),
    paste0(".(i = .I)"),
    paste0(".(g = .GRP)"),
    paste0("list(", numeric_col, ")"),
    paste0("if (.N > 1L) .(a = .N) else .(a = 0L)"),
    paste0(".(", column, " = ", column, ")")
  )
}

gen_assign <- function(spec) {
  column <- col_of(spec)
  numeric_col <- col_of(spec, c("h", "x", "y")) %||% column
  new <- pick(c("z", "z2", "p"))
  switch(sample.int(13L, 1L),
    paste0(new, " := ", numeric_col, " * 2"),
    paste0(new, " := .N"),
    paste0(new, " := ", literal_for(column)),
    paste0(column, " := ", column),
    paste0(numeric_col, " := ", numeric_col, " + 1L"),
    paste0(numeric_col, " := NULL"),
    paste0("c(\"p\", \"q\") := .(", numeric_col, ", ", column, ")"),
    paste0("`:=`(p = ", numeric_col, " + 1, q = .N)"),
    paste0(new, " := sum(", numeric_col, ", na.rm = TRUE)"),
    paste0(new, " := as.character(", column, ")"),
    paste0(new, " := rev(", numeric_col, ")"),
    paste0(new, " := NULL"),
    paste0(new, " := cumsum(", numeric_col, ")")
  )
}

gen_by <- function(spec) {
  column <- col_of(spec, c("g", "h", "f", "b", "x")) %||% col_of(spec)
  second <- col_of(spec, c("g", "h", "f", "b"))
  switch(sample.int(10L, 1L),
    paste0("by = ", column),
    paste0("by = .(", column, ")"),
    paste0("by = .(", column, ", ", second %||% column, ")"),
    paste0("by = \"", column, "\""),
    paste0("by = c(\"", column, "\", \"", second %||% column, "\")"),
    paste0("keyby = ", column),
    paste0("keyby = .(", column, ", ", second %||% column, ")"),
    paste0("by = .(k = ", column, ")"),
    paste0("by = .(", column, " > ", literal_for(if (column == "g") "h" else column), ")"),
    paste0("by = ", column, ", .SDcols = ", pick(c("c(\"x\")", "!c(\"g\")", "is.numeric")))
  )
}

gen_query <- function(spec) {
  shape <- sample.int(14L, 1L)
  q <- switch(shape,
    paste0("dt[", gen_filter(spec), "]"),
    paste0("dt[, ", gen_j(spec, FALSE), "]"),
    paste0("dt[", gen_filter(spec), ", ", gen_j(spec, FALSE), "]"),
    paste0("dt[, ", gen_j(spec, TRUE), ", ", gen_by(spec), "]"),
    paste0("dt[", gen_filter(spec), ", ", gen_j(spec, TRUE), ", ", gen_by(spec), "]"),
    paste0("dt[, ", gen_assign(spec), "]"),
    paste0("dt[", gen_filter(spec), ", ", gen_assign(spec), "]"),
    paste0("dt[, ", gen_assign(spec), ", ", gen_by(spec), "]"),
    paste0("dt[", gen_join_i(spec), "]"),
    paste0("dt[", gen_join_i(spec), ", ", gen_j(spec, FALSE), "]"),
    paste0("dt[", gen_join_i(spec), ", ", gen_j(spec, TRUE), ", by = .EACHI]"),
    paste0("dt[, ", paste0("c(", paste0("\"", spec$columns[seq_len(min(2L, length(spec$columns)))], "\"", collapse = ", "), ")"), ", with = FALSE]"),
    paste0("dt[", gen_filter(spec), "][, ", gen_j(spec, FALSE), "]"),
    paste0("o[dt, on = ", pick(c("\"g\"", "\"h\"", "c(\"g\", \"h\")")), "]")
  )
  q
}

# One run of a query, with everything observable recorded.
observe <- function(query, spec) {
  tables <- build_tables(spec)
  environment_ <- new.env(parent = globalenv())
  environment_$dt <- tables$dt
  environment_$o <- tables$o
  environment_$lim <- 2L
  conditions <- list()
  record <- function(cond) {
    call <- conditionCall(cond)
    conditions[[length(conditions) + 1L]] <<- list(
      class(cond), conditionMessage(cond),
      if (is.null(call)) NULL else deparse(call)
    )
  }
  visible <- NA
  result <- NULL
  printed <- character()
  failed <- FALSE
  printed <- capture.output({
    outcome <- tryCatch(
      withCallingHandlers(
        withVisible(eval(parse(text = query)[[1L]], environment_)),
        warning = function(w) { record(w); invokeRestart("muffleWarning") },
        message = function(m) { record(m); invokeRestart("muffleMessage") }
      ),
      error = function(e) { record(e); failed <<- TRUE; NULL }
    )
    if (!is.null(outcome)) {
      result <<- outcome$value
      visible <<- outcome$visible
      if (isTRUE(outcome$visible)) print(result)
    }
  })
  list(
    result = describe(result), visible = visible, failed = failed,
    printed = printed, conditions = conditions,
    dt = describe(environment_$dt), o = describe(environment_$o)
  )
}

# Values compare by their parts, so an external pointer (the self reference of a data.table)
# never reaches `identical()`, whose answer for it would be the address. Whether the table is
# still self-referential, and how much room it has, are compared instead.
describe <- function(x) {
  if (is.data.table(x)) {
    attributes_ <- attributes(x)
    attributes_[[".internal.selfref"]] <- NULL
    list(
      "data.table", attributes_, lapply(seq_along(x), function(i) x[[i]]),
      data.table:::selfrefok(x), truelength(x)
    )
  } else if (is.list(x) && !is.object(x)) {
    lapply(x, describe)
  } else {
    x
  }
}

same <- function(a, b) identical(a, b, num.eq = FALSE, single.NA = FALSE, attrib.as.set = FALSE, ignore.bytecode = FALSE, ignore.environment = FALSE)

# The first place two values differ, as a path of list positions, for the mismatch report.
first_difference <- function(a, b, path = "") {
  if (same(a, b)) return(NULL)
  if (is.list(a) && is.list(b) && length(a) == length(b) && !is.object(a)) {
    labels <- if (is.null(names(a))) seq_along(a) else names(a)
    for (i in seq_along(a)) {
      found <- first_difference(a[[i]], b[[i]], paste0(path, "[[", labels[[i]], "]]"))
      if (!is.null(found)) return(found)
    }
    return(list(path = paste0(path, " (attributes)"), on = attributes(a), off = attributes(b)))
  }
  list(path = path, on = a, off = b)
}

field_names <- c("result", "visible", "failed", "printed", "conditions", "dt", "o")

run_one <- function(index) {
  set.seed(seed * 1000003 + index)
  spec <- describe_tables()
  query <- gen_query(spec)
  options(ry.datatable = TRUE, ry.datatable.trace = TRUE)
  trace <- file(tempfile(), "w+")
  sink(trace, type = "message")
  on <- observe(query, spec)
  sink(type = "message")
  trace_lines <- readLines(trace)
  close(trace)
  options(ry.datatable.trace = FALSE, ry.datatable = FALSE)
  off <- observe(query, spec)
  options(ry.datatable = TRUE)
  answered <- any(startsWith(trace_lines, "[ry]"))
  differing <- field_names[!vapply(field_names, function(field) same(on[[field]], off[[field]]), logical(1L))]
  list(query = query, answered = answered, differing = differing, on = on, off = off, spec = spec)
}

mismatches <- 0L
answered_count <- 0L
declined_count <- 0L
indices <- if (exists("only", inherits = FALSE) && !is.null(only)) only else seq_len(count)
for (index in indices) {
  outcome <- run_one(index)
  if (outcome$answered) answered_count <- answered_count + 1L else declined_count <- declined_count + 1L
  if (length(outcome$differing)) {
    mismatches <- mismatches + 1L
    cat(sprintf("MISMATCH replay: seed=%d count=%d index=%d\n  query: %s\n  differs in: %s\n",
      seed, count, index, outcome$query, paste(outcome$differing, collapse = ", ")))
    cat("  tables:", paste(capture.output(str(outcome$spec[c("n", "columns", "key")])), collapse = " "), "\n")
    for (field in outcome$differing) {
      found <- first_difference(outcome$on[[field]], outcome$off[[field]], field)
      cat("  at ", found$path, "\n  bridge on:  ", sep = "")
      cat(substr(paste(capture.output(str(found$on, vec.len = 4, give.attr = FALSE)), collapse = " | "), 1, 400), "\n")
      cat("  bridge off: ")
      cat(substr(paste(capture.output(str(found$off, vec.len = 4, give.attr = FALSE)), collapse = " | "), 1, 400), "\n")
    }
  }
}
cat(sprintf("summary: seed=%d queries=%d answered=%d declined=%d mismatches=%d\n",
  seed, length(indices), answered_count, declined_count, mismatches))
