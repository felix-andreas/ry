# Decides, under GNU R and after replay.R, which samples of one capture directory are sound
# benchmark programs, and calibrates them. `hot.py validate` runs it once per directory
# (RY_HOT_CAPTURE), in a process whose library path holds the corpus library but not the code
# bases, so a sample that needs the code base's own namespace fails here. hot.py also points
# TMPDIR and R's user directories at a directory of the process's own, which this script watches.
#
# A sample is read as the harness reads it (replay.R, `relocate`), and it is valid when it
# replays as it ran (an error when it threw, a value otherwise), prints at most a couple of lines,
# opens no connection and no graphics device, leaves the global environment, the options and the
# search path as they were (apart from `.Random.seed`), and takes at most 250 ms a call. Its
# result must be the value the capture kept. The exception is a result that differs between two
# replays in a row (it holds a counter or a generated id): the sample stays valid with the
# comparison `varies`, and the harness prints `varies` in place of its checksum.
#
# Files: a call may create, change and remove files only under the session's temporary
# directory, and a second call must leave the same paths as the first, so that thousands of
# calls in a batch replay the same work. A sample that touches a path anywhere else under the
# watched directory, or whose second call leaves other paths, is dropped.
#
# A sample whose call changes its own variables, such as data.table's `:=`, an R6 object's fields
# or a logger's appenders, is `fresh`: the harness gives it a copy of its variables for every call.
# The check serializes the variables, replays once and serializes them again, with GNU R's JIT
# off so that compiling a closure does not count as a change.
#
# Every package the code base imports (<capture>/session.txt) is loaded before the first sample,
# as the harness loads them, so a sample is judged in the session it will be replayed in.
#
# It writes <capture>/validated.tsv, one row per sample, and for each valid sample a compact copy
# of its replay inputs in <capture>/validated/, with the result's checksum (replay.R) in place of
# the result. The copy holds the call and variables as captured and leaves out the capture's
# timing, so the same call captured in two code bases gives the same file, which `hot.py select`
# keeps once.
local({
  capture <- Sys.getenv("RY_HOT_CAPTURE")
  slowest <- 0.25
  # A sample that runs past this is stopped; it is far beyond `slowest` already.
  give_up <- 5
  # Calibration repeats a call until this long has passed, and a timed batch lasts about as long.
  batch_seconds <- 0.02
  # The harness's figure for a program is the median of this many batches.
  batches <- 5L
  allowed_lines <- 2L

  home <- tempdir()
  watched <- dirname(home)
  if (!nzchar(Sys.getenv("TMPDIR")) || normalizePath(Sys.getenv("TMPDIR")) != normalizePath(watched)) {
    stop("validate.R watches the directory TMPDIR names, which must be its own; run it through `hot.py validate`")
  }
  skipped <- .hot$enter_session(readLines(file.path(capture, "session.txt")))
  if (length(skipped)) cat("the session runs without", paste(skipped, collapse = ", "), "\n")
  samples <- read.delim(file.path(capture, "samples.tsv"), colClasses = "character", quote = "",
                        na.strings = character(), comment.char = "")
  out <- file.path(capture, "validated")
  unlink(out, recursive = TRUE)
  dir.create(out)
  # Samples run in a working directory under the temporary directory, as in the harness, so a
  # file one writes to a relative path is seen and allowed.
  scratch <- tempfile("hot-validate-")
  dir.create(scratch)
  setwd(scratch)
  output_path <- file.path(scratch, "stdout.txt")
  messages_path <- file.path(scratch, "stderr.txt")

  one_line <- function(text) substr(gsub("[[:space:]]+", " ", paste(text, collapse = " ")), 1L, 200L)
  invalid <- function(reason) list(valid = FALSE, reason = one_line(reason))
  paths <- function() {
    sort(list.files(watched, recursive = TRUE, all.files = TRUE, include.dirs = TRUE, full.names = TRUE, no.. = TRUE))
  }
  inside_home <- function(path) startsWith(path, paste0(home, "/"))

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

  # The variables of a sample's environment as bytes, to tell whether a call changed them.
  snapshot <- function(env) serialize(mget(sort(ls(env, all.names = TRUE)), envir = env), NULL)

  # One call of the sample in `env`, with its effects observed: the value or the error, what it
  # printed, the connections and graphics devices it left open, the paths it created or removed,
  # whether it drew random numbers and how long it took. It undoes what it can of the effects on
  # the session (connections, devices, global variables, options), so a dropped sample does not
  # change how a later one replays, but it leaves the files for the next call to find.
  observe <- function(sample, env) {
    if (!is.null(sample$seed)) assign(".Random.seed", sample$seed, envir = globalenv())
    seed_before <- get0(".Random.seed", envir = globalenv(), inherits = FALSE)
    output <- file(output_path, open = "w")
    messages <- file(messages_path, open = "w")
    globals <- setdiff(ls(globalenv(), all.names = TRUE), ".Random.seed")
    values <- mget(globals, envir = globalenv())
    options_before <- options()
    search_before <- search()
    # Not showConnections(), which collects garbage on every call.
    connections <- getAllConnections()
    devices <- dev.list()
    before <- paths()
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
    new_devices <- setdiff(dev.list(), devices)
    for (device in new_devices) dev.off(device)
    outcome$devices <- length(new_devices)
    outcome$paths <- paths()
    changed <- c(setdiff(outcome$paths, before), setdiff(before, outcome$paths))
    outcome$elsewhere <- changed[!inside_home(changed)]
    names_after <- setdiff(ls(globalenv(), all.names = TRUE), ".Random.seed")
    outcome$global <- !identical(names_after, globals) || !identical(mget(names_after, envir = globalenv()), values) ||
      !identical(search(), search_before)
    rm(list = setdiff(names_after, globals), envir = globalenv())
    for (name in globals) assign(name, values[[name]], envir = globalenv())
    outcome$options <- !identical(options(), options_before)
    if (outcome$options) options(options_before)
    outcome$rng <- !identical(seed_before, get0(".Random.seed", envir = globalenv(), inherits = FALSE))
    outcome
  }

  effects <- function(outcome) {
    if (isTRUE(grepl("time limit", outcome$message))) return(sprintf("runs longer than %g s", give_up))
    if (length(outcome$printed) > allowed_lines) return(sprintf("prints %d lines", length(outcome$printed)))
    if (outcome$connections) return("opens a connection")
    if (outcome$devices) return("opens a graphics device")
    if (length(outcome$elsewhere)) return(paste("touches a path outside its temporary directory:", outcome$elsewhere[[1L]]))
    if (outcome$global) return("changes the global environment or the search path")
    if (outcome$options) return("changes the options")
    NULL
  }

  # The seconds one call takes, as the harness measures it: batches prepared as the harness
  # prepares them, of a size that lasts at least `batch_seconds`, and the median of `batches`.
  calibrate <- function(ready) {
    sink(nullfile())
    on.exit(sink())
    timed <- function(count) {
      ready$iterations <- count
      prepared <- .hot$prepare(list(ready))
      started <- as.numeric(Sys.time())
      .hot$run(prepared)
      as.numeric(Sys.time()) - started
    }
    count <- 1L
    repeat {
      elapsed <- timed(count)
      if (elapsed >= batch_seconds || count >= 1e6) break
      count <- count * 2L
    }
    stats::median(vapply(seq_len(batches), function(batch) timed(count), 0)) / count
  }

  check <- function(path, row) {
    sample <- tryCatch(withCallingHandlers(.hot$relocate(readRDS(path)), warning = function(condition) stop(condition)),
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
    # A call that changes its variables in place changes the objects `sample` holds too, so every
    # later replay starts from these bytes.
    bytes <- serialize(sample$variables, NULL)
    pristine <- function() {
      sample$variables <- unserialize(bytes)
      sample
    }
    env <- tryCatch(.hot$environment_of(pristine()), error = function(error) error)
    if (inherits(env, "error")) return(invalid(paste("an import does not resolve:", conditionMessage(env))))
    level <- compiler::enableJIT(0L)
    before <- snapshot(env)
    first <- observe(sample, env)
    fresh <- !identical(snapshot(env), before)
    compiler::enableJIT(level)
    problem <- effects(first)
    if (!is.null(problem)) return(invalid(problem))
    if (first$threw != isTRUE(sample$threw)) {
      return(invalid(if (first$threw) paste("fails:", first$message) else "returns where the capture ended in an error"))
    }
    if (first$seconds > slowest) return(invalid(sprintf("takes %.3f s a call", first$seconds)))
    second <- observe(sample, if (fresh) .hot$environment_of(pristine()) else env)
    problem <- effects(second)
    if (!is.null(problem)) return(invalid(problem))
    if (!identical(second$paths, first$paths)) {
      differing <- c(setdiff(second$paths, first$paths), setdiff(first$paths, second$paths))
      return(invalid(paste("a second call leaves other paths than the first:", differing[[1L]])))
    }
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
    if (rng && is.null(sample$seed)) return(invalid("draws random numbers without a captured seed"))
    seconds <- calibrate(.hot$ready(pristine(), 1L, rng, first$threw, fresh))
    varies <- comparison == "varies"
    checksum <- if (varies) "varies" else .hot$checksum(if (first$threw) .hot$failed else first$value)
    captured <- readRDS(path)
    saveRDS(list(package = captured$package, name = captured$name, call = captured$call, variables = captured$variables,
                 imports = captured$imports, seed = if (rng) captured$seed, threw = first$threw, varies = varies,
                 checksum = checksum),
            file.path(out, basename(path)), compress = "xz")
    list(valid = TRUE, reason = "", comparison = comparison, rng = rng, threw = first$threw, fresh = fresh,
         namespaces = namespaces, seconds = seconds, iterations = max(1L, as.integer(round(batch_seconds / seconds))),
         checksum = checksum)
  }

  rows <- vector("list", nrow(samples))
  for (i in seq_len(nrow(samples))) {
    row <- samples[i, ]
    path <- file.path(capture, row$file)
    if (!file.exists(path)) path <- file.path(capture, "samples", row$file)
    # The files a sample leaves, and the directories its relocation made, go before the next.
    existing <- paths()
    verdict <- tryCatch(check(path, row), error = function(error) invalid(paste("the validator failed:", conditionMessage(error))))
    unlink(setdiff(paths(), existing), recursive = TRUE)
    rows[[i]] <- data.frame(
      file = row$file, package = row$package, name = row$name, valid = verdict$valid, reason = verdict$reason,
      comparison = if (is.null(verdict$comparison)) "" else verdict$comparison,
      rng = isTRUE(verdict$rng), threw = isTRUE(verdict$threw), fresh = isTRUE(verdict$fresh),
      namespaces = paste(verdict$namespaces, collapse = ","),
      gnu_seconds_per_call = if (is.null(verdict$seconds)) NA_real_ else signif(verdict$seconds, 6),
      iterations = if (is.null(verdict$iterations)) NA_integer_ else verdict$iterations,
      bytes = file.size(path),
      checksum_kind = if (is.null(verdict$checksum)) "" else sub(":.*", "", verdict$checksum),
      checksum = if (is.null(verdict$checksum)) "" else verdict$checksum,
      stringsAsFactors = FALSE
    )
    cat(sprintf("%s %s::%s %s\n", row$file, row$package, row$name,
                if (verdict$valid) sprintf("valid (%s%s, %.3g ms a call)", rows[[i]]$comparison,
                                           if (verdict$fresh) ", fresh" else "", verdict$seconds * 1000)
                else paste("invalid:", verdict$reason)))
  }
  table <- do.call(rbind, rows)
  if (is.null(table)) table <- data.frame(file = character(), package = character(), name = character())
  write.table(table, file.path(capture, "validated.tsv"), sep = "\t", quote = FALSE, row.names = FALSE, na = "")
  setwd(capture)
  unlink(scratch, recursive = TRUE)
})
