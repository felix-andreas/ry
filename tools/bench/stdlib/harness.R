# The case runner that `ry.R` and `gnu.R` share. Both run one case per process:
#
#   ry run ry.R harness.R <case> <n> <budget>
#   Rscript gnu.R harness.R <case> <n> <budget>
#
# A case has an untimed setup and one or more named timed expressions, its alternatives. Every
# run of an alternative starts from a fresh setup, so an expression may mutate what setup built.
# An alternative runs up to three times and keeps its best elapsed time, stopping early once its
# runs together exceed `budget` seconds. The case prints one tab-separated line for its fastest
# alternative: `case  label  seconds  checksum  note`. The label is `none` for a case without
# alternatives, and `missing <pkg>` when every alternative needs a package that is not installed;
# when only some do, the note names the packages whose alternatives were skipped.
#
# The checksum summarizes the leaves of the result as `count:total:bytes`: how many scalar leaves
# it has, the sum of its numeric and logical leaves, and the UTF-8 bytes of its string leaves. It
# ignores names, nesting and order, so an R equivalent that returns a vector where ry returns a
# list, or an environment where ry returns a dictionary, still sums to the same value. An `:Ok`
# or `:Err` tag counts as its payload, a bare tag such as `:NotFound` as the string `"NotFound"`,
# and a dictionary or environment as its values.

arguments <- commandArgs(trailingOnly = TRUE)
selected <- arguments[[2]]
n <- as.integer(as.numeric(arguments[[3]]))
budget <- as.numeric(arguments[[4]])

case <- function(name, setup = NULL, ...) {
  if (name != selected) return(invisible())
  setup <- substitute(setup)
  alternatives <- eval(substitute(alist(...)))
  if (length(alternatives) == 0L) {
    cat(sprintf("%s\tnone\tNA\t\t\n", name))
    return(invisible())
  }
  best_label <- NULL
  best_seconds <- Inf
  checksums <- character()
  missing <- character()
  errors <- character()
  for (label in names(alternatives)) {
    package <- if (grepl("::", label, fixed = TRUE)) sub("::.*$", "", label) else NULL
    if (!is.null(package) && !requireNamespace(package, quietly = TRUE)) {
      missing <- c(missing, package)
      next
    }
    outcome <- tryCatch(time_alternative(setup, alternatives[[label]]), error = function(condition) {
      conditionMessage(condition)
    })
    if (is.character(outcome)) {
      errors <- c(errors, sprintf("%s: %s", label, gsub("[\t\n]+", " ", outcome)))
      next
    }
    checksums[[label]] <- outcome$checksum
    if (outcome$seconds < best_seconds) {
      best_seconds <- outcome$seconds
      best_label <- label
    }
  }
  notes <- c(
    if (length(unique(checksums)) > 1L) {
      sprintf("alternatives disagree: %s", paste(names(checksums), checksums, sep = "=", collapse = " "))
    },
    errors,
    if (length(missing) > 0L && !is.null(best_label)) sprintf("skipped, missing %s", paste(unique(missing), collapse = ", "))
  )
  note <- paste(notes, collapse = "; ")
  if (is.null(best_label)) {
    label <- if (length(errors) > 0L) "error" else sprintf("missing %s", paste(unique(missing), collapse = ", "))
    cat(sprintf("%s\t%s\tNA\t\t%s\n", name, label, note))
    return(invisible())
  }
  cat(sprintf("%s\t%s\t%.4f\t%s\t%s\n", name, best_label, best_seconds, checksums[[best_label]], note))
  invisible()
}

time_alternative <- function(setup, expression) {
  best <- Inf
  spent <- 0
  for (run in 1:3) {
    environment <- new.env(parent = globalenv())
    eval(setup, environment)
    invisible(gc())
    seconds <- system.time(result <- eval(expression, environment))[["elapsed"]]
    best <- min(best, seconds)
    spent <- spent + seconds
    if (spent > budget) break
  }
  list(seconds = best, checksum = checksum(result))
}

checksum <- function(value) {
  summary <- summarize(value)
  sprintf("%.0f:%.9g:%.0f", summary[[1]], summary[[2]], summary[[3]])
}

summarize <- function(value) {
  if (inherits(value, "ry_tag")) {
    parts <- as.list(value)
    if (length(parts$payload) == 0L) return(c(1, 0, nchar(parts$tag, type = "bytes")))
    return(summarize(parts$payload))
  }
  if (inherits(value, "ry_dict")) return(summarize(Dict.values(value)))
  if (inherits(value, "ry_record") || inherits(value, "ry_tuple")) return(summarize(as.list(value)))
  if (is.environment(value)) return(summarize(as.list(value, all.names = TRUE)))
  if (is.null(value)) return(c(0, 0, 0))
  if (is.list(value)) {
    if (length(value) == 0L) return(c(0, 0, 0))
    # A list of Results, such as `List.map(s, Str.to_integer)`, is summarized through its
    # payloads in one pass, because a recursive call per tag dominates the run on a million.
    if (inherits(value[[1L]], "ry_tag")) {
      payloads <- tryCatch(
        list(List.keep_oks(value, identity), List.keep_errs(value, identity)),
        error = function(condition) NULL
      )
      if (!is.null(payloads)) return(summarize(payloads[[1L]]) + summarize(payloads[[2L]]))
    }
    # A list of records with the same fields, such as what `Str.split_first` returns, is
    # summarized one field at a time for the same reason.
    if (inherits(value[[1L]], "ry_record")) {
      fields <- names(as.list(value[[1L]]))
      columns <- tryCatch(lapply(fields, function(field) {
        read <- as.function(c(alist(record = ), call("$", quote(record), as.name(field))))
        lapply(value, read)
      }), error = function(condition) NULL)
      if (!is.null(columns)) return(Reduce(`+`, lapply(columns, summarize), c(0, 0, 0)))
    }
    # Flattening is safe for numbers and logicals, whose sum survives the coercion `unlist`
    # applies to a mix of them, and for strings alone, but a string mixed in with numbers
    # would turn every number into text.
    if (all(lengths(value) == 1L)) {
      flat <- unlist(value, use.names = FALSE)
      if (is.atomic(flat) && length(flat) == length(value) &&
        (!is.character(flat) || all(vapply(value, is.character, NA)))) {
        return(summarize(flat))
      }
    }
    types <- unique(vapply(value, scalar_type, ""))
    if (length(types) == 1L && types != "") return(summarize(unlist(value, use.names = FALSE)))
    total <- c(0, 0, 0)
    for (element in value) total <- total + summarize(element)
    return(total)
  }
  if (is.character(value)) return(c(length(value), 0, sum(nchar(value, type = "bytes"))))
  c(length(value), sum(as.numeric(value)), 0)
}

scalar_type <- function(element) {
  if (is.object(element) || length(element) != 1L) return("")
  type <- typeof(element)
  if (type %in% c("logical", "integer", "double", "character")) type else ""
}

# Inputs that both sides build the same way. `scrambled` is a permutation of 1..n in a fixed
# order, so sorting has work to do without relying on either engine's random number generator.
doubles <- function(count) as.double(seq_len(count))
scrambled <- function(count) as.double((seq_len(count) * 7919) %% count + 1)
keys <- function(count) sprintf("k%d", seq_len(count))
words <- function(count) sprintf("item_%07d_value", seq_len(count))
padded <- function(count) sprintf("  item %d  ", seq_len(count))
