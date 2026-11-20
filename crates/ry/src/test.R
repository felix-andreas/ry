# `ry test`: runs a package's testthat files against the package `load_all()` loaded, the way
# `devtools::test()` does, in worker processes when asked to, and with `watch` reruns them after
# every edit, the tests an edit can affect first.

local({
  test_directory <- function(root) file.path(root, "tests", "testthat")

  test_files <- function(root) {
    sort(list.files(test_directory(root), pattern = "^test.*\\.[rR]$", full.names = TRUE))
  }

  # testthat's name for a test file, which a filter matches: the file name without a leading
  # `test-` or `test_` and without the extension.
  context_name <- function(files) sub("[.][Rr]$", "", sub("^test[-_]", "", basename(files)))

  file_filter <- function(files) {
    stems <- context_name(files)
    paste0("^(", paste(gsub("([][{}()+*^$|\\\\?.])", "\\\\\\1", stems), collapse = "|"), ")$")
  }

  run <- function(root, package, files, reporter) {
    if (!length(files)) return(NULL)
    arguments <- list(
      test_directory(root), filter = file_filter(files), package = package,
      load_package = "none", stop_on_failure = FALSE, stop_on_warning = FALSE
    )
    if (!is.null(reporter)) arguments$reporter <- reporter
    # Set while workers start, by the `queue_setup()` of `load_workers_with_ry`.
    on.exit(Sys.unsetenv("RY_RUN_FLAGS"))
    do.call(testthat::test_dir, arguments)
  }

  # testthat decides from the environment whether the files run in worker processes and in how
  # many: `TESTTHAT_PARALLEL` overrides the package's `Config/testthat/parallel`, and
  # `TESTTHAT_CPUS` or the `Ncpus` option, which wins, names the count, which is 2 otherwise.
  # `jobs` is the user's `--jobs`, which settles both; without it the package decides whether,
  # and a count nothing names is every core. The count is set only for a run that starts
  # workers, so that tests in one process see the settings they would see under devtools.
  # Returns whether the run starts workers.
  choose_workers <- function(root, jobs, cores) {
    if (!is.null(jobs)) Sys.setenv(TESTTHAT_PARALLEL = if (jobs > 1L) "TRUE" else "FALSE")
    if (!wants_workers(root)) return(FALSE)
    if (!is.null(jobs)) {
      Sys.setenv(TESTTHAT_CPUS = jobs)
      options(Ncpus = jobs)
    } else if (is.null(getOption("Ncpus")) && !nzchar(Sys.getenv("TESTTHAT_CPUS"))) {
      Sys.setenv(TESTTHAT_CPUS = cores)
    }
    TRUE
  }

  # Whether testthat will start workers for a run of more than one file, by its own rule: the
  # environment variable decides when it is set, and the package's `Config/testthat/parallel`
  # counts only together with the third edition.
  wants_workers <- function(root) {
    setting <- Sys.getenv("TESTTHAT_PARALLEL")
    if (nzchar(setting)) return(identical(toupper(setting), "TRUE"))
    fields <- read.dcf(file.path(root, "DESCRIPTION"),
                       fields = c("Config/testthat/parallel", "Config/testthat/edition"))
    identical(toupper(fields[[1L]]), "TRUE") && isTRUE(as.integer(fields[[2L]]) >= 3L)
  }

  # testthat's worker processes load the package with `pkgload::load_all()`, which sources
  # every file and refuses a package with an import that is not installed. On ry's runtime a
  # worker loads it the way this process did instead: lazily, and under the project's switches
  # (`switches`, which ry's `bin/R` reads from `RY_RUN_FLAGS`; the worker forgets them so that
  # an R process a test starts gets the defaults, as in a run without workers). testthat builds
  # the code a worker runs from its profile inside `queue_setup()`, with rlang's quasiquotation,
  # so the call is replaced in that function's body. testthat sources the helpers in each
  # worker itself, so the load leaves them out, as the call it replaces does. A testthat whose
  # `queue_setup()` holds no such call keeps its own workers.
  load_workers_with_ry <- function(switches) {
    namespace <- asNamespace("testthat")
    setup <- get0("queue_setup", envir = namespace, mode = "function", inherits = FALSE)
    load <- call("{", quote(Sys.unsetenv("RY_RUN_FLAGS")), .ry_attach_call,
                 quote(load_all(!!test_dir, quiet = TRUE, helpers = FALSE)))
    replaced <- FALSE
    replace <- function(code) {
      if (identical(code[[1L]], quote(pkgload::load_all))) {
        replaced <<- TRUE
        return(load)
      }
      for (i in seq_along(code)) {
        if (is.call(code[[i]])) code[[i]] <- replace(code[[i]])
      }
      code
    }
    if (!is.null(setup)) {
      body(setup) <- call("{", bquote(Sys.setenv(RY_RUN_FLAGS = .(switches))), replace(body(setup)))
    }
    if (!replaced) {
      message("This testthat's worker processes cannot use ry's loader, so they load the ",
              "package with pkgload.")
      return(invisible())
    }
    replace_binding(namespace, "queue_setup", setup)
  }

  replace_binding <- function(namespace, name, value) {
    unlockBinding(name, namespace)
    assign(name, value, envir = namespace)
    lockBinding(name, namespace)
  }

  # On Unix, testthat's worker queue is replaced by workers forked from this process, which has
  # testthat and the package loaded already, so a worker starts without loading either and
  # without a callr session. Each worker runs `queue_process_setup()` (helpers, setup files) and
  # then takes one file at a time from its standard input, as a callr worker does, and sends the
  # reporter events back over its pipe, a test at a time. It gets its own `tempdir()` and random
  # seed, as a callr worker would, and once every file has run, the queue's teardown asks each
  # idle worker to run the teardown and exit. The callr workers stay for a testthat whose
  # internals differ from the ones driven here, and for a process that may have started a thread
  # pool (a table builtin's Polars query, or the polars package), which a forked child inherits
  # without its threads and would wait on forever.
  # (`decisions/tooling-bench-ci.md`, "ry test forks its workers")
  fork_workers <- function() {
    namespace <- asNamespace("testthat")
    setup <- get0("queue_setup", envir = namespace, mode = "function", inherits = FALSE)
    arguments <- c("test_paths", "test_package", "test_dir", "num_workers", "load_helpers",
                   "load_package", "shuffle")
    internals <- c("queue_process_setup", "queue_task", "queue_teardown", "PROCESS_MSG",
                   "PROCESS_OUTPUT")
    if (is.null(setup) || !identical(names(formals(setup)), arguments) ||
        !all(vapply(internals, exists, NA, envir = namespace, inherits = FALSE))) {
      return(invisible())
    }
    teardown <- get("queue_teardown", envir = namespace)
    replace_binding(namespace, "queue_setup", function(test_paths, test_package, test_dir,
                                                       num_workers, load_helpers, load_package,
                                                       shuffle = FALSE) {
      if (.Internal(ryThreadPoolStarted()) || "polars" %in% loadedNamespaces()) {
        return(setup(test_paths, test_package, test_dir, num_workers, load_helpers,
                     load_package, shuffle))
      }
      fork_queue(test_paths, test_package %||% Sys.getenv("TESTTHAT_PKG"), test_dir,
                 num_workers, load_package, shuffle)
    })
    replace_binding(namespace, "queue_teardown", function(queue) {
      if (inherits(queue, "ry_fork_queue")) queue$teardown() else teardown(queue)
    })
  }

  fork_queue <- function(paths, test_package, test_dir, workers, load_package, shuffle) {
    testthat <- asNamespace("testthat")
    hyperlinks <- cli::ansi_has_hyperlink_support()
    # A worker's file: `NA` while it starts, its file while it runs one, and "" once no file is
    # left for it.
    files <- character()
    pids <- integer()
    temps <- character()
    next_path <- 1L
    teardown_error <- NULL
    child <- function() {
      # Whatever ends the worker, it never returns into the parent's code it was forked from.
      on.exit(parallel:::mcexit(1L))
      status <- tryCatch({
        work(test_package, test_dir, load_package, shuffle, hyperlinks)
        0L
      }, error = function(e) 1L, interrupt = function(e) 1L)
      parallel:::mcexit(status)
    }
    stop_worker <- function(index) {
      tools::pskill(pids[[index]], tools::SIGTERM)
      parallel:::rmChild(pids[[index]])
      pids[[index]] <<- NA_integer_
      if (!is.na(temps[[index]])) unlink(temps[[index]], recursive = TRUE)
    }
    forked <- FALSE
    on.exit(if (!forked) for (index in which(!is.na(pids))) stop_worker(index))
    for (index in seq_len(workers)) {
      process <- parallel:::mcfork()
      if (inherits(process, "masterProcess")) child()
      pids[[index]] <- process$pid
      files[[index]] <- NA_character_
      temps[[index]] <- NA_character_
    }
    forked <- TRUE
    assign_next <- function(index) {
      if (next_path > length(paths)) {
        files[[index]] <<- ""
        return()
      }
      files[[index]] <<- paths[[next_path]]
      next_path <<- next_path + 1L
      parallel:::sendChildStdin(pids[[index]], paste0(files[[index]], "\n"))
    }
    failed <- function(index, error, output = character()) {
      file <- files[[index]]
      if (is.na(file)) {
        output <- gsub("}", "}}", gsub("{", "{{", output, fixed = TRUE), fixed = TRUE)
        cli::cli_abort(c("testthat subprocess failed to start.", " " = "{conditionMessage(error)}",
                         structure(output, names = rep(" ", length(output)))),
                       test_file = NULL, class = c("testthat_process_error", "testthat_error"),
                       call = NULL)
      }
      cli::cli_abort("testthat subprocess exited in file {.file {file}}.", test_file = file,
                     parent = error, class = c("testthat_process_error", "testthat_error"),
                     call = NULL)
    }
    receive <- function(index) {
      message <- parallel:::readChild(pids[[index]])
      if (!is.raw(message)) {
        exited <- pids[[index]]
        pids[[index]] <<- NA_integer_
        if (!identical(files[[index]], "")) {
          failed(index, simpleError(paste("the worker process", exited, "exited")))
        }
        return(list())
      }
      message <- unserialize(message)
      if (identical(message$kind, "ready")) temps[[index]] <<- message$temp %||% NA_character_
      if (!is.null(message$error)) {
        if (identical(message$kind, "teardown")) {
          teardown_error <<- teardown_error %||% message$error
        } else {
          failed(index, message$error, message$output)
        }
      }
      if (identical(message$kind, "ready")) {
        assign_next(index)
        return(list())
      }
      events <- lapply(message$events, function(event) {
        list(code = testthat$PROCESS_MSG, message = event)
      })
      if (length(message$output)) {
        events[[length(events) + 1L]] <- structure(
          list(code = testthat$PROCESS_OUTPUT, message = message$output,
               path = files[[index]]),
          class = "testthat_message"
        )
      }
      if (identical(message$kind, "done")) assign_next(index)
      events
    }
    queue <- new.env(parent = emptyenv())
    queue$poll <- function(timeout = 0) {
      limit <- proc.time()[[3L]] + timeout
      repeat {
        alive <- pids[!is.na(pids)]
        if (!length(alive)) return(list())
        wait <- if (is.finite(timeout)) max(limit - proc.time()[[3L]], 0) else -1
        ready <- parallel:::selectChildren(alive, wait)
        if (!is.integer(ready)) return(list())
        events <- unlist(lapply(match(ready, pids), receive), recursive = FALSE)
        if (length(events)) return(events)
        if (is.finite(timeout) && proc.time()[[3L]] >= limit) return(list())
      }
    }
    queue$is_idle <- function() all(is.na(pids) | (!is.na(files) & files == ""))
    queue$list_tasks <- function() data.frame()
    # As testthat's own teardown: an idle worker runs the teardown, with a second's grace for
    # all of them, and a worker still busy, after an error, is stopped.
    queue$teardown <- function() {
      for (index in which(!is.na(pids))) {
        if (identical(files[[index]], "")) {
          tryCatch(parallel:::sendChildStdin(pids[[index]], "\n"),
                   error = function(e) stop_worker(index))
        } else {
          stop_worker(index)
        }
      }
      limit <- proc.time()[[3L]] + 1
      repeat {
        alive <- pids[!is.na(pids)]
        wait <- limit - proc.time()[[3L]]
        if (!length(alive) || wait <= 0) break
        ready <- parallel:::selectChildren(alive, wait)
        if (!is.integer(ready)) break
        for (index in match(ready, pids)) receive(index)
      }
      for (index in which(!is.na(pids))) stop_worker(index)
      if (!is.null(teardown_error)) {
        cli::cli_abort("At least one parallel worker failed to run teardown",
                       parent = teardown_error)
      }
    }
    class(queue) <- "ry_fork_queue"
    queue
  }

  # The body of a forked worker. Its reporter events are sent a test at a time, and what a file
  # writes to the console is sent with the file's last batch, as callr's workers report it. What
  # the setup writes is shown only when it fails, and what the teardown writes is dropped, as
  # testthat does with its own workers.
  work <- function(test_package, test_dir, load_package, shuffle, hyperlinks) {
    testthat <- asNamespace("testthat")
    send <- function(kind, events = list(), output = character(), error = NULL, temp = NULL) {
      message <- list(kind = kind, events = events, output = output, error = error, temp = temp)
      parallel:::sendMaster(serialize(message, NULL, xdr = FALSE))
    }
    capture <- function(code) {
      output <- textConnection(NULL, "w", local = TRUE)
      sink(output)
      sink(output, type = "message")
      error <- tryCatch({
        code
        NULL
      }, error = function(e) e)
      sink(type = "message")
      sink()
      lines <- textConnectionValue(output)
      close(output)
      list(error = error, lines = lines)
    }
    if (exists(".Random.seed", envir = globalenv(), inherits = FALSE)) {
      rm(".Random.seed", envir = globalenv())
    }
    temp <- NULL
    setup <- capture({
      temp <- .Internal(ryWorkerTempDir())
      if (identical(load_package, "installed")) library(test_package, character.only = TRUE)
      options(cli.hyperlink = hyperlinks, cli.hyperlink_run = hyperlinks)
      testthat$queue_process_setup(test_package = test_package, test_dir = test_dir,
                                   load_helpers = TRUE, load_package = "none")
    })
    send("ready", output = setup$lines, error = setup$error, temp = temp)
    if (!is.null(setup$error)) return(invisible())
    input <- file("stdin")
    open(input)
    repeat {
      path <- readLines(input, n = 1L)
      if (!length(path) || !nzchar(path)) break
      events <- list()
      record <- function(event) {
        events[[length(events) + 1L]] <<- event
        if (event$cmd %in% c("end_test", "end_context")) {
          send("events", events)
          events <<- list()
        }
      }
      task <- capture(withCallingHandlers(testthat$queue_task(path, shuffle),
                                          testthat_message = record))
      send("done", events, task$lines, task$error)
    }
    teardown <- capture(withr::deferred_run(.GlobalEnv))
    send("teardown", error = teardown$error)
  }

  failed <- function(results) {
    if (is.null(results)) return(FALSE)
    summary <- as.data.frame(results)
    any(summary$failed > 0L) || any(summary$error)
  }

  mentions <- new.env(parent = emptyenv())
  mentioned <- function(file) {
    if (!is.null(mentions[[file]])) return(mentions[[file]])
    names <- tryCatch(unique(all.names(parse(file, keep.source = FALSE))), error = function(e) "")
    assign(file, names, envir = mentions)
    names
  }

  # The test files an edit can affect: the ones edited, and the ones that mention a package name
  # whose definition reaches a rebound or removed name. `NULL` when anything else changed (a
  # helper, a fixture, `inst/`, `src/`, the metadata), notifications were lost, or the reload
  # ran code, because then any test can be affected.
  affected <- function(root, edited, changed) {
    is_test <- dirname(edited) == "tests/testthat" & grepl("^test.*\\.[rR]$", basename(edited))
    if (is.null(changed) || anyNA(edited) || !all(is_test | startsWith(edited, "R/"))) {
      return(NULL)
    }
    edited_tests <- file.path(root, edited[is_test])
    for (file in intersect(edited_tests, ls(mentions))) rm(list = file, envir = mentions)
    files <- test_files(root)
    callers <- if (length(changed)) .ry_callers(root, changed) else character()
    files[files %in% edited_tests | vapply(files, function(file) {
      any(mentioned(file) %in% callers)
    }, NA)]
  }

  function(root, filter, reporter, watch, jobs, cores, switches) {
    package <- read.dcf(file.path(root, "DESCRIPTION"), fields = "Package")[[1L]]
    # As `devtools::test()`: a developer's run is not a CRAN check, so a missing package is an
    # error rather than a skip, and `skip_on_cran()` tests run.
    if (!nzchar(Sys.getenv("NOT_CRAN"))) Sys.setenv(NOT_CRAN = "true")
    if (choose_workers(root, jobs, cores) && !is.null(.ry_attach_call)) {
      load_workers_with_ry(switches)
      if (.Platform$OS.type == "unix") fork_workers()
    }
    selected <- function() {
      files <- test_files(root)
      if (is.null(filter)) files else files[grepl(filter, context_name(files))]
    }
    # Watching starts before the first run, so an edit made during it is not missed.
    if (watch) .ry_wait_edits(root, 0)
    results <- run(root, package, selected(), reporter)
    if (!watch) return(!failed(results))
    for (file in selected()) mentioned(file)
    message("Watching for changes; press Ctrl-C to stop.")
    repeat {
      # A short timeout keeps the wait interruptible: R handles Ctrl-C between two waits.
      edited <- .ry_wait_edits(root, 0.25)
      if (!length(edited)) next
      reloaded <- tryCatch({
        load_all(root, quiet = TRUE)
        TRUE
      }, error = function(e) {
        message("Reloading failed: ", conditionMessage(e))
        FALSE
      })
      if (!reloaded) next
      changed <- .ry_packages[[.ry_find_root(root)]]$changed
      first <- affected(root, edited, changed)
      files <- selected()
      if (is.null(first)) {
        run(root, package, files, reporter)
      } else {
        first <- intersect(first, files)
        run(root, package, first, reporter)
        run(root, package, setdiff(files, first), reporter)
      }
    }
  }
})
