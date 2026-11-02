# `ry test`: runs a package's testthat files against the package `load_all()` loaded, the way
# `devtools::test()` does, and with `watch` reruns them after every edit, the tests an edit can
# affect first.

local({
  test_directory <- function(root) file.path(root, "tests", "testthat")

  test_files <- function(root) {
    sort(list.files(test_directory(root), pattern = "^test.*\\.[rR]$", full.names = TRUE))
  }

  # Helpers and setup files fill the environment every test file runs in, so a change to one
  # can affect any test.
  shared_files <- function(root) {
    list.files(test_directory(root), pattern = "^(helper|setup).*\\.[rR]$", full.names = TRUE)
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

  # Only sources: a run itself creates and deletes files such as `_snaps/`, which would
  # otherwise retrigger the watch.
  stamps <- function(root) {
    paths <- c(
      list.files(file.path(root, "R"), pattern = "\\.[RrSsq]$", recursive = TRUE, full.names = TRUE),
      list.files(test_directory(root), pattern = "\\.[rR]$", full.names = TRUE),
      file.path(root, c("DESCRIPTION", "NAMESPACE"))
    )
    info <- file.info(paths, extra_cols = FALSE)
    stats::setNames(paste(info$size, as.numeric(info$mtime)), paths)
  }

  mentions <- new.env(parent = emptyenv())
  mentioned <- function(file, stamp) {
    cached <- mentions[[file]]
    if (!is.null(cached) && identical(cached$stamp, stamp)) return(cached$names)
    names <- tryCatch(unique(all.names(parse(file, keep.source = FALSE))), error = function(e) NULL)
    assign(file, list(stamp = stamp, names = names), envir = mentions)
    names
  }

  # The test files an edit can affect: the ones that changed, and the ones that mention a
  # package name whose definition reaches a rebound or removed name. `NULL` when the reload
  # ran code or a helper changed, which can affect every test.
  affected <- function(root, before, after, changed) {
    edited <- names(after)[is.na(match(after, before[names(after)]))]
    if (is.null(changed) || any(edited %in% shared_files(root))) return(NULL)
    files <- test_files(root)
    callers <- if (length(changed)) .ry_callers(root, changed) else character()
    hit <- vapply(files, function(file) {
      file %in% edited || any(mentioned(file, after[[file]]) %in% callers)
    }, NA)
    files[hit]
  }

  function(root, filter, reporter, watch) {
    package <- read.dcf(file.path(root, "DESCRIPTION"), fields = "Package")[[1L]]
    selected <- function() {
      files <- test_files(root)
      if (is.null(filter)) files else files[grepl(filter, basename(files))]
    }
    results <- run(root, package, selected(), reporter)
    if (!watch) return(!failed(results))
    before <- stamps(root)
    for (file in selected()) mentioned(file, before[[file]])
    message("Watching for changes; press Ctrl-C to stop.")
    repeat {
      Sys.sleep(0.2)
      after <- stamps(root)
      if (identical(after, before)) next
      ok <- tryCatch({
        load_all(root, quiet = TRUE)
        TRUE
      }, error = function(e) {
        message("Reloading failed: ", conditionMessage(e))
        FALSE
      })
      if (ok) {
        changed <- .ry_packages[[.ry_find_root(root)]]$changed
        first <- affected(root, before, after, changed)
        files <- selected()
        if (is.null(first)) {
          run(root, package, files, reporter)
        } else {
          first <- intersect(first, files)
          run(root, package, first, reporter)
          run(root, package, setdiff(files, first), reporter)
        }
      }
      before <- after
    }
  }
})
