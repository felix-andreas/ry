# `ry test`: runs a package's testthat files against the package `load_all()` loaded, the way
# `devtools::test()` does, and with `watch` reruns them after every edit, the tests an edit can
# affect first.

local({
  test_directory <- function(root) file.path(root, "tests", "testthat")

  test_files <- function(root) {
    sort(list.files(test_directory(root), pattern = "^test.*\\.[rR]$", full.names = TRUE))
  }

  # testthat filters files by their name without `test-` and the extension.
  file_filter <- function(files) {
    stems <- sub("\\.[rR]$", "", sub("^test-?", "", basename(files)))
    paste0("^(", paste(gsub("([][{}()+*^$|\\\\?.])", "\\\\\\1", stems), collapse = "|"), ")$")
  }

  run <- function(root, package, files, reporter) {
    if (!length(files)) return(NULL)
    arguments <- list(
      test_directory(root), filter = file_filter(files), package = package,
      load_package = "none", stop_on_failure = FALSE, stop_on_warning = FALSE
    )
    if (!is.null(reporter)) arguments$reporter <- reporter
    do.call(testthat::test_dir, arguments)
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

  function(root, filter, reporter, watch) {
    package <- read.dcf(file.path(root, "DESCRIPTION"), fields = "Package")[[1L]]
    # As `devtools::test()`: a developer's run is not a CRAN check, so a missing package is an
    # error rather than a skip, and `skip_on_cran()` tests run.
    if (!nzchar(Sys.getenv("NOT_CRAN"))) Sys.setenv(NOT_CRAN = "true")
    selected <- function() {
      files <- test_files(root)
      if (is.null(filter)) files else files[grepl(filter, basename(files))]
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
