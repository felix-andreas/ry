# Decides, under GNU R and after replay.R, which samples of one capture directory are sound
# benchmark programs, and calibrates them. `hot.py validate` runs it once per directory
# (RY_HOT_CAPTURE), in a process whose library path holds the corpus library but not the code
# bases, so a sample that needs the code base's own namespace fails here.
#
# A sample is valid when it replays as it ran (an error when it threw, a value otherwise), prints
# at most a couple of lines, opens no connection, creates no file, leaves the global environment,
# the options and the search path as they were (apart from `.Random.seed`), and takes at most
# 250 ms a call. Its result must be the value the capture kept. The exception is a result that
# differs between two replays in a row (it holds a counter or a generated id): the sample stays
# valid with the comparison `varies`, and the harness prints `varies` in place of its checksum.
#
# Every package the code base imports (<capture>/session.txt) is loaded before the first sample,
# as the harness loads them, so a sample is judged in the session it will be replayed in.
#
# It writes <capture>/validated.tsv, one row per sample, and for each valid sample a compact copy
# of its replay inputs in <capture>/validated/, with the result's checksum (replay.R) in place of
# the result unless the result is small. The copy leaves out the capture's timing, so the same
# call captured in two code bases gives the same file, which `hot.py select` keeps once.
local({
  capture <- Sys.getenv("RY_HOT_CAPTURE")
  slowest <- 0.25
  # A sample that runs past this is stopped; it is far beyond `slowest` already.
  give_up <- 5
  # Calibration repeats a call until this long has passed, and a timed batch lasts about as long.
  batch_seconds <- 0.02
  allowed_lines <- 2L
  kept_value_bytes <- 4096

  skipped <- .hot$enter_session(readLines(file.path(capture, "session.txt")))
  if (length(skipped)) cat("the session runs without", paste(skipped, collapse = ", "), "\n")
  samples <- read.delim(file.path(capture, "samples.tsv"), colClasses = "character", quote = "",
                        na.strings = character(), comment.char = "")
  out <- file.path(capture, "validated")
  unlink(out, recursive = TRUE)
  dir.create(out)
  # Samples run in an empty working directory, so a file one writes there is seen.
  scratch <- tempfile("hot-validate-")
  dir.create(scratch)
  setwd(scratch)
  output_path <- file.path(scratch, "stdout.txt")
  messages_path <- file.path(scratch, "stderr.txt")

  one_line <- function(text) substr(gsub("[[:space:]]+", " ", paste(text, collapse = " ")), 1L, 200L)
  invalid <- function(reason) list(valid = FALSE, reason = one_line(reason))

  # The namespaces a replay needs: the entry's package, the packages it imports names from, every
  # `pkg::name` in the call, and every namespace a captured closure or environment refers to.
  namespaces_of <- function(sample) {
    found <- c(sample$package, unname(sample$imports))
    walk_call <- function(x) {
      if (is.call(x)) {
        head <- x[[1L]]
        if (is.symbol(head) && as.character(head) %in% c("::", ":::")) found <<- c(found, as.character(x[[2L]]))
        # An empty argument, as in `x[i, ]`, is the missing argument, which cannot be passed on.
        for (k in seq_along(x)) if (!identical(x[[k]], quote(expr = ))) walk_call(x[[k]])
      } else if (is.function(x) && !is.primitive(x)) {
        walk_call(body(x))
      }
    }
    seen <- list()
    walk_value <- function(x, depth) {
      if (depth > 50L) return()
      if (is.environment(x)) {
        while (!identical(x, emptyenv()) && !identical(x, globalenv())) {
          if (isNamespace(x)) {
            found <<- c(found, getNamespaceName(x))
            return()
          }
          if (any(vapply(seen, identical, TRUE, x))) return()
          seen[[length(seen) + 1L]] <<- x
          # A call's frame binds `...` and each formal that got no argument to the missing
          # argument, which cannot be passed on.
          for (name in setdiff(ls(x, all.names = TRUE), "...")) {
            if (!identical(get(name, envir = x), quote(expr = ))) walk_value(get(name, envir = x), depth + 1L)
          }
          x <- parent.env(x)
        }
      } else if (is.function(x) && !is.primitive(x)) {
        walk_call(body(x))
        walk_value(environment(x), depth + 1L)
      } else if (is.list(x)) {
        # A list of formals holds the missing argument for each one without a default.
        for (k in seq_along(x)) if (!identical(x[[k]], quote(expr = ))) walk_value(x[[k]], depth + 1L)
      }
      for (attribute in attributes(x)) if (is.environment(attribute) || is.function(attribute)) walk_value(attribute, depth + 1L)
    }
    walk_call(sample$call)
    walk_value(sample$variables, 0L)
    found <- unique(found[nzchar(found)])
    sort(setdiff(found, "base"))
  }

  # A copy of a value with every environment, function and external pointer replaced by its
  # type: those compare by address, so two equal results would never be identical with them.
  # The list is rebuilt without its class, so no `[[<-` method of a data frame or table runs.
  addressed <- c("environment", "closure", "builtin", "special", "externalptr", "weakref", "bytecode")
  strip <- function(x) {
    type <- typeof(x)
    if (type %in% addressed) return(paste0("<", type, ">"))
    found <- attributes(x)
    if (type == "list") {
      attributes(x) <- NULL
      x <- lapply(x, strip)
    }
    if (length(found) || type == "list") {
      stripped <- lapply(found, strip)
      if (type == "list" || !identical(stripped, found)) attributes(x) <- stripped
    }
    x
  }
  opaque <- function(x) {
    if (typeof(x) %in% addressed) return(TRUE)
    if (typeof(x) == "list") for (item in unclass(x)) if (opaque(item)) return(TRUE)
    for (attribute in attributes(x)) if (opaque(attribute)) return(TRUE)
    FALSE
  }

  global_state <- function() {
    names <- setdiff(ls(globalenv(), all.names = TRUE), ".Random.seed")
    list(values = mget(names, envir = globalenv()), options = options(), search = search())
  }

  # One call of the sample in `env`, with its effects observed: the value or the error, what it
  # printed, the connections and files it left, whether it drew random numbers and how long it took.
  observe <- function(sample, env) {
    if (!is.null(sample$seed)) assign(".Random.seed", sample$seed, envir = globalenv())
    seed_before <- get0(".Random.seed", envir = globalenv(), inherits = FALSE)
    output <- file(output_path, open = "w")
    messages <- file(messages_path, open = "w")
    state <- global_state()
    # Not showConnections(), which collects garbage on every call.
    connections <- getAllConnections()
    files <- c(dir(tempdir(), recursive = TRUE, all.files = TRUE), dir(scratch, recursive = TRUE, all.files = TRUE))
    sink(output)
    sink(messages, type = "message")
    started <- as.numeric(Sys.time())
    outcome <- tryCatch({
      setTimeLimit(elapsed = give_up, transient = TRUE)
      value <- .hot$handled(eval(sample$call, env))
      setTimeLimit(elapsed = Inf)
      list(value = value, threw = FALSE)
    }, error = function(error) {
      setTimeLimit(elapsed = Inf)
      list(value = NULL, threw = TRUE, message = conditionMessage(error))
    }, finally = {
      sink(type = "message")
      sink()
    })
    outcome$seconds <- as.numeric(Sys.time()) - started
    close(output)
    close(messages)
    outcome$printed <- c(readLines(output_path, warn = FALSE), readLines(messages_path, warn = FALSE))
    opened <- setdiff(getAllConnections(), connections)
    for (number in opened) try(close(getConnection(number)), silent = TRUE)
    outcome$connections <- length(opened)
    after <- c(dir(tempdir(), recursive = TRUE, all.files = TRUE), dir(scratch, recursive = TRUE, all.files = TRUE))
    created <- setdiff(after, files)
    unlink(c(file.path(tempdir(), created), file.path(scratch, created)), recursive = TRUE)
    outcome$files <- created
    after_state <- global_state()
    outcome$global <- !identical(after_state$values, state$values) || !identical(after_state$search, state$search)
    outcome$options <- !identical(after_state$options, state$options)
    if (outcome$options) options(state$options)
    outcome$rng <- !identical(seed_before, get0(".Random.seed", envir = globalenv(), inherits = FALSE))
    outcome
  }

  effects <- function(outcome) {
    if (isTRUE(grepl("time limit", outcome$message))) return(sprintf("runs longer than %g s", give_up))
    if (length(outcome$printed) > allowed_lines) return(sprintf("prints %d lines", length(outcome$printed)))
    if (outcome$connections) return("opens a connection")
    if (length(outcome$files)) return(paste("creates the file", outcome$files[[1L]]))
    if (outcome$global) return("changes the global environment or the search path")
    if (outcome$options) return("changes the options")
    NULL
  }

  # The seconds one call takes in a batch, the least of three batches of a size that lasts at
  # least `batch_seconds`.
  calibrate <- function(prepared) {
    sink(nullfile())
    on.exit(sink())
    timed <- function(count) {
      prepared$iterations <- count
      started <- as.numeric(Sys.time())
      .hot$run(list(prepared))
      as.numeric(Sys.time()) - started
    }
    count <- 1L
    repeat {
      elapsed <- timed(count)
      if (elapsed >= batch_seconds || count >= 1e6) break
      count <- count * 2L
    }
    min(elapsed, timed(count), timed(count)) / count
  }

  check <- function(path, row) {
    sample <- tryCatch(withCallingHandlers(readRDS(path), warning = function(condition) stop(condition)),
                       error = function(error) error)
    if (inherits(sample, "error")) return(invalid(paste("does not load:", conditionMessage(sample))))
    namespaces <- namespaces_of(sample)
    for (namespace in namespaces) {
      problem <- tryCatch({
        suppressPackageStartupMessages(loadNamespace(namespace))
        NULL
      }, error = function(error) conditionMessage(error))
      if (!is.null(problem)) return(invalid(paste("namespace", namespace, "does not load:", problem)))
    }
    env <- tryCatch(.hot$environment_of(sample), error = function(error) error)
    if (inherits(env, "error")) return(invalid(paste("an import does not resolve:", conditionMessage(env))))
    first <- observe(sample, env)
    problem <- effects(first)
    if (!is.null(problem)) return(invalid(problem))
    if (first$threw != isTRUE(sample$threw)) {
      return(invalid(if (first$threw) paste("fails:", first$message) else "returns where the capture ended in an error"))
    }
    if (first$seconds > slowest) return(invalid(sprintf("takes %.3f s a call", first$seconds)))
    second <- observe(sample, env)
    problem <- effects(second)
    if (!is.null(problem)) return(invalid(problem))
    if (second$threw != first$threw) return(invalid("fails on one replay and not the other"))
    comparison <- "error"
    if (!first$threw) {
      value <- strip(first$value)
      addresses <- opaque(first$value)
      # A result that holds a process-wide counter or a generated id differs on every call. The
      # call is still part of the workload, so it stays, and its result goes unchecked.
      varies <- !identical(value, strip(second$value))
      if (varies) {
        comparison <- "varies"
      } else if (isTRUE(sample$value_kept)) {
        kept <- strip(sample$value)
        if (identical(value, kept)) {
          comparison <- if (addresses) "opaque" else "identical"
        } else if (isTRUE(all.equal(value, kept))) {
          comparison <- if (addresses) "opaque" else "all.equal"
        } else {
          return(invalid("differs from the value the capture kept"))
        }
      } else {
        comparison <- if (addresses) "opaque" else "deterministic"
      }
    }
    rng <- first$rng
    prepared <- list(call = sample$call, env = env, seed = if (rng) sample$seed, threw = first$threw, iterations = 1L)
    if (rng && is.null(sample$seed)) return(invalid("draws random numbers without a captured seed"))
    seconds <- calibrate(prepared)
    varies <- comparison == "varies"
    checksum <- if (varies) "varies" else .hot$checksum(if (first$threw) .hot$failed else first$value)
    small <- !first$threw && !varies && as.numeric(object.size(first$value)) <= kept_value_bytes
    saveRDS(list(package = sample$package, name = sample$name, call = sample$call, variables = sample$variables,
                 imports = sample$imports, seed = if (rng) sample$seed, threw = first$threw,
                 value = if (small) first$value, value_kept = small, varies = varies, checksum = checksum),
            file.path(out, basename(path)), compress = "xz")
    list(valid = TRUE, reason = "", comparison = comparison, rng = rng, threw = first$threw,
         namespaces = namespaces, seconds = seconds, iterations = max(1L, as.integer(round(batch_seconds / seconds))),
         checksum = checksum)
  }

  rows <- vector("list", nrow(samples))
  for (i in seq_len(nrow(samples))) {
    row <- samples[i, ]
    path <- file.path(capture, row$file)
    if (!file.exists(path)) path <- file.path(capture, "samples", row$file)
    verdict <- tryCatch(check(path, row), error = function(error) invalid(paste("the validator failed:", conditionMessage(error))))
    rows[[i]] <- data.frame(
      file = row$file, package = row$package, name = row$name, valid = verdict$valid, reason = verdict$reason,
      comparison = if (is.null(verdict$comparison)) "" else verdict$comparison,
      rng = isTRUE(verdict$rng), threw = isTRUE(verdict$threw),
      namespaces = paste(verdict$namespaces, collapse = ","),
      gnu_seconds_per_call = if (is.null(verdict$seconds)) NA_real_ else signif(verdict$seconds, 6),
      iterations = if (is.null(verdict$iterations)) NA_integer_ else verdict$iterations,
      bytes = file.size(path),
      checksum_kind = if (is.null(verdict$checksum)) "" else sub(":.*", "", verdict$checksum),
      checksum = if (is.null(verdict$checksum)) "" else verdict$checksum,
      stringsAsFactors = FALSE
    )
    cat(sprintf("%s %s::%s %s\n", row$file, row$package, row$name,
                if (verdict$valid) sprintf("valid (%s, %.3g ms a call)", rows[[i]]$comparison, verdict$seconds * 1000)
                else paste("invalid:", verdict$reason)))
  }
  table <- do.call(rbind, rows)
  if (is.null(table)) table <- data.frame(file = character(), package = character(), name = character())
  write.table(table, file.path(capture, "validated.tsv"), sep = "\t", quote = FALSE, row.names = FALSE, na = "")
  setwd(capture)
  unlink(scratch, recursive = TRUE)
})
