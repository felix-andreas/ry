# A base-R model of ry's DataFrame API as `reference/dataframes.mdx` documents it, the reference
# of the frame fuzz kind under GNU R. A frame is a list of columns and a row count; an expression
# is a function from the columns (and their row count) to a value, the name of the first column
# it reads, and whether it reduces the rows to one value. The fuzz kind never generates a shape
# the contract leaves open, so the model need not define one either.

# The contract compares and sorts text by its bytes, which is R's C collation.
invisible(Sys.setlocale("LC_COLLATE", "C"))

.model_frame <- function(columns, rows, class = "DataFrame") {
  structure(list(columns = columns, rows = as.integer(rows)), class = class)
}

.model_is_frame <- function(x) inherits(x, "DataFrame") || inherits(x, "LazyFrame")

.model_subject <- function(x) {
  if (!.model_is_frame(x)) stop("expected a DataFrame or a LazyFrame")
  x
}

.model_restyle <- function(frame, like) {
  class(frame) <- class(like)
  frame
}

DataFrame <- function(...) {
  columns <- list(...)
  if (length(columns) && (is.null(names(columns)) || any(names(columns) == ""))) {
    stop("every column needs a name")
  }
  lengths <- vapply(columns, length, 0L)
  rows <- if (length(lengths)) max(lengths) else 0L
  for (index in seq_along(columns)) {
    if (lengths[index] == 1L) columns[[index]] <- rep(columns[[index]], rows)
    else if (lengths[index] != rows) stop("columns differ in length")
  }
  .model_frame(columns, rows)
}

as.DataFrame <- function(x) {
  columns <- lapply(as.list(x), function(column) { attributes(column) <- NULL; column })
  do.call(DataFrame, columns)
}

is.DataFrame <- function(x) inherits(x, "DataFrame")

as.data.frame.DataFrame <- function(x, ...) {
  structure(x$columns, class = "data.frame", row.names = .set_row_names(x$rows))
}

DataFrame.height <- function(df) .model_subject(df)$rows
DataFrame.width <- function(df) length(.model_subject(df)$columns)
DataFrame.columns <- function(df) names(.model_subject(df)$columns)
DataFrame.schema <- function(df) {
  vapply(.model_subject(df)$columns, function(column) paste0(typeof(column), "[]"), "")
}

DataFrame.lazy <- function(df) .model_restyle(.model_subject(df), structure(0, class = "LazyFrame"))
LazyFrame.collect <- function(lf) .model_restyle(.model_subject(lf), structure(0, class = "DataFrame"))

# Expressions.

.model_expr <- function(evaluate, name, reduces = FALSE, head = FALSE) {
  structure(list(evaluate = evaluate, name = name, reduces = reduces, head = head), class = "Expr")
}

.model_as_expr <- function(x) {
  if (inherits(x, "Expr")) return(x)
  if (is.character(x) && length(x) == 1L) return(Expr.col(x))
  stop("expected an expression")
}

.model_operand <- function(x) {
  if (inherits(x, "Expr")) return(x)
  value <- x
  .model_expr(function(columns, rows) value, NULL)
}

Expr.col <- function(name) {
  force(name)
  .model_expr(function(columns, rows) {
    if (!name %in% names(columns)) stop("there is no column ", name)
    columns[[name]]
  }, name)
}

Expr.lit <- function(value) {
  force(value)
  .model_expr(function(columns, rows) rep(value, rows), NULL)
}

Expr.alias <- function(expr, name) {
  expr$name <- name
  expr
}

Ops.Expr <- function(e1, e2) {
  operator <- get(.Generic, mode = "function", envir = baseenv())
  if (missing(e2)) {
    inner <- .model_operand(e1)
    return(.model_expr(function(columns, rows) operator(inner$evaluate(columns, rows)),
                       inner$name, inner$reduces))
  }
  left <- .model_operand(e1)
  right <- .model_operand(e2)
  name <- if (is.null(left$name)) right$name else left$name
  .model_expr(function(columns, rows) {
    operator(left$evaluate(columns, rows), right$evaluate(columns, rows))
  }, name, left$reduces || right$reduces)
}

.model_reduce <- function(reduce) {
  force(reduce)
  function(expr) {
    expr <- .model_as_expr(expr)
    .model_expr(function(columns, rows) reduce(expr$evaluate(columns, rows)), expr$name, TRUE)
  }
}

Expr.sum <- .model_reduce(sum)
Expr.mean <- .model_reduce(mean)
Expr.median <- .model_reduce(median)
Expr.min <- .model_reduce(min)
Expr.max <- .model_reduce(max)
Expr.first <- .model_reduce(function(values) values[1L])
Expr.last <- .model_reduce(function(values) values[length(values)])
Expr.std <- .model_reduce(sd)
Expr.var <- .model_reduce(var)

Expr.len <- function() .model_expr(function(columns, rows) as.integer(rows), NULL, TRUE)

Expr.head <- function(expr, n = 10L) {
  expr <- .model_as_expr(expr)
  .model_expr(function(columns, rows) head(expr$evaluate(columns, rows), n), expr$name, TRUE, TRUE)
}

Expr.is_null <- function(expr) {
  expr <- .model_as_expr(expr)
  .model_expr(function(columns, rows) is.na(expr$evaluate(columns, rows)), expr$name, expr$reduces)
}

Expr.if_else <- function(test, yes, no) {
  test <- .model_as_expr(test)
  yes <- .model_operand(yes)
  no <- .model_operand(no)
  name <- Find(Negate(is.null), list(test$name, yes$name, no$name))
  .model_expr(function(columns, rows) {
    chosen <- test$evaluate(columns, rows)
    yes_values <- rep_len(yes$evaluate(columns, rows), rows)
    no_values <- rep_len(no$evaluate(columns, rows), rows)
    out <- no_values
    out[chosen] <- yes_values[chosen]
    out
  }, name)
}

Expr.cor <- function(first, second) {
  first <- .model_as_expr(first)
  second <- .model_as_expr(second)
  .model_expr(function(columns, rows) {
    suppressWarnings(cor(as.double(first$evaluate(columns, rows)),
                         as.double(second$evaluate(columns, rows))))
  }, first$name, TRUE)
}

Expr.rank <- function(expr, descending = FALSE) {
  expr <- .model_as_expr(expr)
  .model_expr(function(columns, rows) {
    ordering <- order(expr$evaluate(columns, rows), decreasing = descending,
                      na.last = TRUE, method = "radix")
    ranks <- integer(rows)
    ranks[ordering] <- seq_len(rows)
    ranks
  }, expr$name)
}

Expr.over <- function(...) {
  arguments <- list(...)
  expr <- .model_as_expr(arguments[[1L]])
  keys <- unlist(arguments[-1L], use.names = FALSE)
  .model_expr(function(columns, rows) {
    groups <- .model_groups(lapply(keys, function(key) Expr.col(key)$evaluate(columns, rows)), rows)
    out <- NULL
    for (members in groups) {
      part <- lapply(columns, `[`, members)
      value <- expr$evaluate(part, length(members))
      if (is.null(out)) out <- rep(value[1L], rows)
      out[members] <- value
    }
    # An empty input still has the type a one-row input gives (`min` of nothing is a double).
    if (is.null(out)) out <- expr$evaluate(lapply(columns, `[`, NA_integer_), 1L)[0L]
    out
  }, expr$name)
}

# Rows with equal keys, as one integer vector per group in the order of each group's first row.
# Equal numbers are one key whatever the sign of a zero.
.model_key <- function(values) {
  if (is.double(values)) sprintf("%a", values + 0)
  else ifelse(is.na(values), "\001NA", as.character(values))
}

.model_groups <- function(keys, rows) {
  if (!rows) return(list())
  text <- do.call(paste, c(lapply(keys, .model_key), sep = "\002"))
  split(seq_len(rows), factor(text, levels = unique(text)))
}

# Verbs.

.model_items <- function(arguments) {
  labels <- names(arguments)
  lapply(seq_along(arguments), function(index) {
    item <- .model_as_expr(arguments[[index]])
    if (!is.null(labels) && labels[index] != "") item$name <- labels[index]
    if (is.null(item$name)) stop("an expression without a column needs a name")
    item
  })
}

.model_columns <- function(frame, items) {
  columns <- lapply(items, function(item) {
    if (item$reduces) stop("an aggregation outside `agg`")
    rep_len(item$evaluate(frame$columns, frame$rows), frame$rows)
  })
  names(columns) <- vapply(items, function(item) item$name, "")
  columns
}

.model_take <- function(frame, rows) {
  .model_restyle(.model_frame(lapply(frame$columns, `[`, rows), length(rows)), frame)
}

DataFrame.select <- function(...) {
  arguments <- list(...)
  frame <- .model_subject(arguments[[1L]])
  columns <- .model_columns(frame, .model_items(arguments[-1L]))
  .model_restyle(.model_frame(columns, frame$rows), frame)
}

DataFrame.with_columns <- function(...) {
  arguments <- list(...)
  frame <- .model_subject(arguments[[1L]])
  added <- .model_columns(frame, .model_items(arguments[-1L]))
  for (name in names(added)) frame$columns[[name]] <- added[[name]]
  frame
}

DataFrame.filter <- function(...) {
  arguments <- list(...)
  frame <- .model_subject(arguments[[1L]])
  keep <- rep(TRUE, frame$rows)
  for (item in .model_items(arguments[-1L])) {
    test <- rep_len(item$evaluate(frame$columns, frame$rows), frame$rows)
    if (!is.logical(test)) stop("a filter needs a logical condition")
    keep <- keep & !is.na(test) & test
  }
  .model_take(frame, which(keep))
}

DataFrame.group_by <- function(...) {
  arguments <- list(...)
  frame <- .model_subject(arguments[[1L]])
  keys <- .model_items(arguments[-1L])
  if (!length(keys)) stop("`group_by` needs at least one key column")
  structure(list(frame = frame, keys = keys), class = "GroupBy")
}

GroupBy.agg <- function(...) {
  arguments <- list(...)
  group <- arguments[[1L]]
  if (!inherits(group, "GroupBy")) stop("`agg` needs the result of `group_by`")
  frame <- group$frame
  aggregates <- .model_items(arguments[-1L])
  key_columns <- .model_columns(frame, group$keys)
  groups <- .model_groups(key_columns, frame$rows)
  out <- c(lapply(key_columns, `[`, integer()),
           lapply(aggregates, function(item) item$evaluate(lapply(frame$columns, `[`, NA_integer_), 1L)[0L]))
  names(out) <- c(names(key_columns), vapply(aggregates, function(item) item$name, ""))
  if (!all(vapply(aggregates, function(item) item$reduces, TRUE))) {
    stop("`agg` needs one value per group")
  }
  rows <- 0L
  for (members in groups) {
    part <- lapply(frame$columns, `[`, members)
    values <- lapply(aggregates, function(item) item$evaluate(part, length(members)))
    # `Expr.head` gives a group as many rows as it keeps, none included.
    width <- if (length(values)) max(vapply(values, length, 0L)) else 1L
    pieces <- c(lapply(key_columns, function(column) rep(column[members[1L]], width)),
                lapply(values, function(value) rep_len(value, width)))
    out <- Map(c, out, pieces)
    rows <- rows + width
  }
  names(out) <- c(names(key_columns), vapply(aggregates, function(item) item$name, ""))
  .model_restyle(.model_frame(out, rows), frame)
}

DataFrame.sort <- function(..., descending = FALSE) {
  arguments <- list(...)
  frame <- .model_subject(arguments[[1L]])
  keys <- .model_columns(frame, .model_items(arguments[-1L]))
  if (!length(keys)) stop("`sort` needs at least one column to sort by")
  if (length(descending) != 1L && length(descending) != length(keys)) stop("bad `descending`")
  ordering <- do.call(order, c(unname(keys), list(decreasing = rep_len(descending, length(keys)),
                                                  na.last = TRUE, method = "radix")))
  .model_take(frame, ordering)
}

DataFrame.head <- function(df, n = 5) {
  frame <- .model_subject(df)
  .model_take(frame, seq_len(min(n, frame$rows)))
}

DataFrame.tail <- function(df, n = 5) {
  frame <- .model_subject(df)
  .model_take(frame, seq_len(min(n, frame$rows)) + frame$rows - min(n, frame$rows))
}

.model_lookup <- function(frame, names) {
  missing <- setdiff(names, names(frame$columns))
  if (length(missing)) stop("there is no column ", missing[1L])
  lapply(names, function(name) frame$columns[[name]])
}

# The right table's columns that join the left's, renamed with `suffix` where the left has the name.
.model_right_columns <- function(left, right, drop, suffix) {
  kept <- right$columns[setdiff(names(right$columns), drop)]
  names(kept) <- ifelse(names(kept) %in% names(left$columns), paste0(names(kept), suffix), names(kept))
  kept
}

DataFrame.join <- function(df, other, on = NULL, how = "inner", left_on = on, right_on = on,
                           suffix = "_right") {
  left <- .model_subject(df)
  right <- .model_subject(other)
  if (how == "cross") {
    pairs <- expand.grid(right = seq_len(right$rows), left = seq_len(left$rows))
    columns <- c(lapply(left$columns, `[`, pairs$left),
                 lapply(.model_right_columns(left, right, character(), suffix), `[`, pairs$right))
    return(.model_restyle(.model_frame(columns, nrow(pairs)), left))
  }
  left_keys <- do.call(paste, c(lapply(.model_lookup(left, left_on), .model_key), sep = "\002"))
  right_keys <- do.call(paste, c(lapply(.model_lookup(right, right_on), .model_key), sep = "\002"))
  if (!left$rows) left_keys <- character()
  if (!right$rows) right_keys <- character()
  matched <- left_keys %in% right_keys
  if (how == "semi") return(.model_take(left, which(matched)))
  if (how == "anti") return(.model_take(left, which(!matched)))
  left_rows <- integer()
  right_rows <- integer()
  for (row in seq_len(left$rows)) {
    partners <- which(right_keys == left_keys[row])
    if (!length(partners) && how %in% c("left", "full")) partners <- NA_integer_
    left_rows <- c(left_rows, rep(row, length(partners)))
    right_rows <- c(right_rows, partners)
  }
  if (how == "full") {
    unmatched <- which(!right_keys %in% left_keys)
    left_rows <- c(left_rows, rep(NA_integer_, length(unmatched)))
    right_rows <- c(right_rows, unmatched)
  } else if (how != "inner" && how != "left") {
    stop("unknown join ", how)
  }
  columns <- lapply(left$columns, `[`, left_rows)
  if (how == "full") {
    for (index in seq_along(left_on)) {
      from_right <- is.na(left_rows)
      columns[[left_on[index]]][from_right] <- right$columns[[right_on[index]]][right_rows[from_right]]
    }
  }
  columns <- c(columns, lapply(.model_right_columns(left, right, right_on, suffix), `[`, right_rows))
  .model_restyle(.model_frame(columns, length(left_rows)), left)
}

DataFrame.join_asof <- function(df, other, on, by = NULL, strategy = "backward",
                                suffix = "_right") {
  left <- .model_subject(df)
  right <- .model_subject(other)
  .model_lookup(left, c(on, by))
  .model_lookup(right, c(on, by))
  key <- function(frame) {
    if (!frame$rows) return(character())
    if (is.null(by)) return(rep("", frame$rows))
    do.call(paste, c(lapply(frame$columns[by], .model_key), sep = "\002"))
  }
  left_keys <- key(left)
  right_keys <- key(right)
  chosen <- vapply(seq_len(left$rows), function(row) {
    value <- left$columns[[on]][row]
    candidates <- which(right_keys == left_keys[row])
    distance <- right$columns[[on]][candidates] - value
    pick <- switch(strategy,
      backward = candidates[distance <= 0][which.max(distance[distance <= 0])],
      forward = candidates[distance >= 0][which.min(distance[distance >= 0])],
      nearest = candidates[which.min(abs(distance))],
      stop("unknown strategy ", strategy))
    if (length(pick)) pick else NA_integer_
  }, 0L)
  columns <- c(left$columns,
               lapply(.model_right_columns(left, right, c(on, by), suffix), `[`, chosen))
  .model_restyle(.model_frame(columns, left$rows), left)
}

DataFrame.pivot <- function(df, on, index, values, agg = NULL) {
  frame <- .model_subject(df)
  .model_lookup(frame, c(on, index, values))
  combine <- switch(agg, sum = sum, mean = mean, min = min, max = max,
                    first = function(x) x[1L], last = function(x) x[length(x)])
  rows <- sort(unique(frame$columns[[index]]), method = "radix")
  names_out <- sort(unique(frame$columns[[on]]), method = "radix")
  template <- combine(frame$columns[[values]][1L])
  columns <- list(rows)
  for (name in names_out) {
    column <- rep(template[NA_integer_], length(rows))
    for (position in seq_along(rows)) {
      members <- which(frame$columns[[on]] == name & frame$columns[[index]] == rows[position])
      if (length(members)) column[position] <- combine(frame$columns[[values]][members])
    }
    columns[[length(columns) + 1L]] <- column
  }
  names(columns) <- c(index, names_out)
  .model_frame(columns, length(rows))
}

DataFrame.set_key <- function(df, cols) {
  frame <- do.call(DataFrame.sort, c(list(df), as.list(cols)))
  frame$key <- cols
  assign(deparse(substitute(df)), frame, envir = parent.frame())
  invisible(frame)
}

DataFrame.key <- function(df) .model_subject(df)$key
DataFrame.copy <- function(df) .model_subject(df)

for (.model_verb in c("select", "with_columns", "filter", "group_by", "sort", "head", "tail",
                      "join", "join_asof", "pivot")) {
  assign(paste0("LazyFrame.", .model_verb), get(paste0("DataFrame.", .model_verb)))
}
LazyGroupBy.agg <- function(...) {
  arguments <- list(...)
  .model_restyle(do.call(GroupBy.agg, arguments), structure(0, class = "LazyFrame"))
}
rm(.model_verb)
