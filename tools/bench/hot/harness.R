# Runs the hot suite's programs in one process, after replay.R, on GNU R or ry. One process for
# the whole suite is what keeps the suite fast: every package loads once.
#
# Environment:
#   RY_HOT_SUITE     the suite directory, which sample paths are relative to
#   RY_HOT_SESSION   comma-separated packages the code bases import, all loaded before the first
#                    program so that every run replays in the same session (replay.R)
#   RY_HOT_PROGRAMS  the program table bench.py writes: a header, then one tab-separated row per
#                    program with its id and comma-separated lists of its sample files,
#                    iterations, rng, threw and fresh flags (1 or 0) and namespaces
#   RY_BENCH_ROUNDS  timed batches per program (default 5)
#   RY_HOT_MODE      `time` (default), `regions` (each timed batch inside a runtime region named by
#                    the program's id) or `instructions` (one batch inside the region and no clock
#                    call there, for callgrind)
#   RY_HOT_FILTER    a regular expression a program id must match (default: every program)
#
# For each program it loads the namespaces the program needs, reads each sample and rebuilds its
# environment once, runs one untimed warm-up batch, collects the young garbage, and times the
# batches. Before every batch, outside the timing and outside the region, it prepares the
# batch's environments (replay.R, `prepare`). It prints
#   <id> <checksums> <min batch seconds> <median batch seconds> <calls per batch>
# where the checksums are those of each sample's last result (`varies` for a sample the
# validator found to give a different result on every call), joined by commas, or
#   <id> failed <reason>
# when a namespace does not load, a sample cannot be read, or a call fails that should not.
#
# The calls run in a directory of their own under `tempdir()`, so a file a call writes to a
# relative path lands under the session's temporary directory, as it did in the validator.
local({
  suite <- Sys.getenv("RY_HOT_SUITE")
  table <- Sys.getenv("RY_HOT_PROGRAMS")
  mode <- Sys.getenv("RY_HOT_MODE", "time")
  filter <- Sys.getenv("RY_HOT_FILTER")
  rounds <- suppressWarnings(as.integer(Sys.getenv("RY_BENCH_ROUNDS", "5")))
  if (is.na(rounds) || rounds < 1L) stop("RY_BENCH_ROUNDS must be a positive whole number")
  if (!mode %in% c("time", "regions", "instructions")) stop("RY_HOT_MODE must be time, regions or instructions, not ", mode)

  # The region call exists only on ry; under GNU R, and when only timing, a region is nothing.
  region <- if (mode != "time" && exists(".ry_bench_region")) get(".ry_bench_region") else function(name) invisible(NULL)
  # Sys.time() steps by a few microseconds on both engines; proc.time() only by the millisecond.
  now <- function() as.numeric(Sys.time())
  items <- function(text) strsplit(text, ",", fixed = TRUE)[[1L]]
  one_line <- function(text) gsub("[[:space:]]+", " ", paste(text, collapse = " "))

  skipped <- .hot$enter_session(items(Sys.getenv("RY_HOT_SESSION")))
  if (length(skipped)) cat("# session without ", paste(skipped, collapse = ","), "\n", sep = "")
  rows <- strsplit(readLines(table)[-1L], "\t", fixed = TRUE)
  work <- tempfile("hot-")
  dir.create(work)
  setwd(work)
  for (row in rows) {
    id <- row[[1L]]
    if (nzchar(filter) && !grepl(filter, id)) next
    # A trailing empty field (no namespaces) is dropped by strsplit.
    row <- c(row, rep("", 7L - length(row)))
    files <- items(row[[2L]])
    iterations <- as.integer(items(row[[3L]]))
    rng <- items(row[[4L]]) == "1"
    threw <- items(row[[5L]]) == "1"
    fresh <- items(row[[6L]]) == "1"
    failure <- NULL
    for (namespace in items(row[[7L]])) {
      failure <- tryCatch({
        loadNamespace(namespace)
        NULL
      }, error = function(error) paste("namespace", namespace, "does not load:", conditionMessage(error)))
      if (!is.null(failure)) break
    }
    samples <- NULL
    if (is.null(failure)) {
      samples <- tryCatch(
        lapply(seq_along(files), function(k) {
          sample <- .hot$relocate(readRDS(file.path(suite, files[[k]])))
          .hot$ready(sample, iterations[[k]], rng[[k]], threw[[k]], fresh[[k]])
        }),
        error = function(error) {
          failure <<- paste("a sample does not load:", conditionMessage(error))
          NULL
        }
      )
    }
    times <- NA_real_
    results <- NULL
    if (is.null(failure)) {
      inside <- FALSE
      sink(nullfile())
      # One batch: its environments prepared outside the region, its calls inside.
      timed <- function() {
        prepared <- .hot$prepare(samples)
        region(id)
        inside <<- TRUE
        started <- now()
        results <<- .hot$run(prepared)
        elapsed <- now() - started
        region("")
        inside <<- FALSE
        elapsed
      }
      failure <- tryCatch({
        results <- .hot$run(.hot$prepare(samples))
        # A full collection takes GNU R a third of a second with the session's packages loaded.
        invisible(gc(full = FALSE))
        if (mode == "instructions") {
          prepared <- .hot$prepare(samples)
          region(id)
          inside <- TRUE
          results <- .hot$run(prepared)
          region("")
          inside <- FALSE
        } else {
          times <- vapply(seq_len(rounds), function(round) timed(), 0)
        }
        NULL
      }, error = function(error) {
        if (inside) region("")
        paste("a call failed:", conditionMessage(error))
      }, finally = sink())
    }
    if (!is.null(failure)) {
      cat(id, " failed ", one_line(failure), "\n", sep = "")
      next
    }
    checksums <- vapply(results, .hot$checksum, "")
    checksums[vapply(samples, function(sample) sample$varies, NA)] <- "varies"
    checksums <- paste(checksums, collapse = ",")
    cat(sprintf("%s %s %.6f %.6f %d\n", id, checksums, min(times), stats::median(times), sum(iterations)))
  }
})
