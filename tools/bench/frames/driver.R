# Runs one benchmark module's pipeline at scale and prints `step NAME CHECKSUM SECONDS` per step.
# The same driver runs the untyped module (data.table) and the typed one (DataFrame): it only
# calls the module's functions and reads results with `$`, `[[`, `nrow` and `ncol`, which both
# kinds of table answer, so the checksums must agree.
#
# Usage: driver.R <module> <module-file> <rows> <data-dir> <untyped|typed|migrated> [instance-id]
#
# With an instance id, the file may be any generated package file: only the top-level definitions
# whose names end in `_<id>` are evaluated, so the other templates in the file need not run. The
# `migrated` variant is an untyped module after `ry migrate data-table`, whose tables are
# DataFrames but which may still call data.table where no rewrite applied.
args <- commandArgs(trailingOnly = TRUE)
module <- args[[1]]
rows <- as.integer(as.numeric(args[[3]]))
data <- args[[4]]
variant <- args[[5]]
id <- if (length(args) >= 6) args[[6]] else "[0-9]+"
if (variant != "typed") library(data.table)
if (length(args) >= 6) {
  for (expression in parse(args[[2]], keep.source = FALSE)) {
    target <- if (is.call(expression) && identical(expression[[1]], as.name("<-"))) expression[[2]]
    if (is.name(target) && endsWith(as.character(target), paste0("_", id))) eval(expression, globalenv())
  }
} else {
  source(args[[2]])
}

instance <- function(prefix) {
  name <- ls(pattern = paste0("^", prefix, "_", id, "$"), envir = globalenv())[[1]]
  get(name, envir = globalenv())
}
make_table <- if (variant == "untyped") data.table::data.table else DataFrame

step <- function(name, run, checksum) {
  start <- proc.time()[["elapsed"]]
  value <- run()
  seconds <- proc.time()[["elapsed"]] - start
  cat("step", name, format(checksum(value), digits = 15), format(seconds, nsmall = 3), "\n")
  value
}
numeric_sum <- function(table, skip) {
  columns <- setdiff(names(table), skip)
  sum(vapply(columns, function(name) sum(as.numeric(table[[name]]), na.rm = TRUE), 0))
}

if (module == "tables") {
  events <- step("make_events", function() instance("make_events")(rows = rows, groups = 1000L),
    function(x) nrow(x) + sum(x$amount))
  step("summarise_groups", function() instance("summarise_groups")(events),
    function(x) sum(x$n) + round(sum(x$total), 2) + round(sum(x$share_flagged), 6))
  featured <- step("add_features", function() instance("add_features")(events),
    function(x) sum(as.numeric(x$rank_in_group)) + round(sum(abs(x$z), na.rm = TRUE), 4) + sum(x$size_class == "small"))
  labels <- make_table(group = unique(events$group)[-1], label = toupper(unique(events$group)[-1]))
  step("join_labels", function() instance("join_labels")(events, labels),
    function(x) nrow(x) + sum(is.na(x$label)))
  step("reshape_wide", function() instance("reshape_wide")(featured),
    function(x) nrow(x) * 1000 + ncol(x) + round(numeric_sum(x, "group"), 2))
} else if (module == "market_data") {
  symbols <- paste0("SYM", seq_len(100L))
  start <- as.Date("2000-01-01")
  prices <- step("fetch_prices", function() {
    instance("fetch_prices")(symbols, start, start + rows %/% 100L - 1L, source = instance("synthetic_source"))
  }, function(x) nrow(x) + round(sum(x$price), 2) + sum(as.numeric(x$date)))
  step("latest_prices", function() instance("latest_prices")(prices),
    function(x) sum(x$observations) + round(sum(x$price), 4))
  step("to_wide", function() instance("to_wide")(prices),
    function(x) nrow(x) * 1000 + ncol(x) + round(numeric_sum(x, "date"), 2))
  step("read_reference", function() instance("read_reference")(file.path(data, "reference.csv")),
    function(x) nrow(x) + round(sum(x$price), 2) + sum(x$volume))
} else if (module == "trials") {
  enrolment <- step("enrol", function() instance("enrol")(patients = rows %/% 10L),
    function(x) nrow(x) + sum(x$arm == "active") + sum(as.numeric(x$enrolled)))
  schedule <- step("visit_schedule", function() instance("visit_schedule")(enrolment, visits = 10L),
    function(x) nrow(x) + sum(as.numeric(x$study_day)) + sum(as.numeric(x$due)))
  step("visits_per_month", function() instance("visits_per_month")(schedule),
    function(x) nrow(x) + sum(x$N) + sum(as.numeric(x$month)))
} else {
  stop("unknown module ", module)
}
