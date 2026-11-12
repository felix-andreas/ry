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
    unlockBinding("queue_setup", namespace)
    assign("queue_setup", setup, envir = namespace)
    lockBinding("queue_setup", namespace)
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
