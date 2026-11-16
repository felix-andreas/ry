# Times exported functions of base, stats and utils, one case at a time, in one process on GNU R or
# ry. bench.py and sweep.py prepend hot/replay.R, whose `.hot$checksum` folds each case's result the
# way the hot suite folds a replayed call's.
#
# A case file under cases/ is named after its package (`base-*.R`, `stats-*.R`, `utils-*.R`) and
# holds calls of two functions:
#
#   bench("abs", setup = { ... }, scalar = abs(dbl_1), dbl_1e6 = abs(dbl_1e6))
#   skip("file.create", "writes files")
#
# Each named argument of `bench` is one case, `<package>::<function>/<variant>`, whose expression
# runs in a loop. `setup` runs once before the case, outside the clock, in the environment the
# expression runs in, whose parent holds the fixtures below.
#
# Environment:
#   RY_SWEEP_DIR      the directory of the case files
#   RY_SWEEP_MODE     `list` (print the cases, the skips and the exports covered by neither) or
#                     `time` (default)
#   RY_SWEEP_CASES    in time mode, a file with the ids of the cases to time, one per line, in order
#   RY_SWEEP_OUT      the file the lines go to, appended to and flushed one case at a time, so a
#                     process that dies keeps the lines of the cases before it
#   RY_SWEEP_BATCH    the seconds one timed batch of calls should take (default 0.01)
#   RY_SWEEP_BATCHES  timed batches per case (default 5)
#
# In time mode a case's line is `<id> TAB <checksum> TAB <min> TAB <median> TAB <calls>`: the
# fastest and the median seconds per call over the batches, and the calls in one batch. A case
# that fails prints `<id> TAB failed TAB <reason>`. A case whose first call takes longer than
# `slow_call` seconds is timed by that call alone.
local({
  directory <- Sys.getenv("RY_SWEEP_DIR")
  mode <- Sys.getenv("RY_SWEEP_MODE", "time")
  out <- Sys.getenv("RY_SWEEP_OUT")
  batch_seconds <- as.numeric(Sys.getenv("RY_SWEEP_BATCH", "0.01"))
  batches <- as.integer(Sys.getenv("RY_SWEEP_BATCHES", "5"))
  if (!nzchar(directory) || !nzchar(out)) stop("RY_SWEEP_DIR and RY_SWEEP_OUT must be set")
  if (!mode %in% c("list", "time")) stop("RY_SWEEP_MODE must be list or time, not ", mode)
  slow_call <- 2
  packages <- c("base", "stats", "utils")

  # The fixtures: inputs every case may name, each built from a seed of its own the first time a
  # case names it, so a process builds only the ones its cases use and every engine builds the
  # same values whatever order the cases run in. A large fixture is dropped again before a case
  # that does not name it, because the time of a call that allocates depends on how much the
  # collector holds alive, and so every case runs beside the small fixtures and its own inputs.
  fixtures <- new.env(parent = globalenv())
  builders <- environment()
  recipes <- list()
  fixture <- function(name, seed, expression, n = NULL) {
    expression <- substitute(expression)
    if (!is.null(n)) expression <- do.call(substitute, list(expression, list(n = n)))
    recipe <- call("delayedAssign", name, call("{", call("set.seed", seed), expression), builders, fixtures)
    recipes[[name]] <<- recipe
    eval(recipe)
  }
  words <- c("alpha", "beta", "gamma", "delta", "price", "yield", "curve", "bond", "rate", "spread",
             "the", "of", "and", "to", "in", "is", "for", "on", "with", "as")
  sentence <- function(n) {
    lengths <- sample(3:8, n, replace = TRUE)
    vapply(lengths, function(k) paste(sample(words, k, replace = TRUE), collapse = " "), "")
  }
  for (size in c(1e3, 1e6)) {
    tag <- if (size == 1e3) "1e3" else "1e6"
    n <- as.integer(size)
    seed <- if (size == 1e3) 1L else 2L
    local({
      fixture(paste0("dbl_", tag), seed, runif(n, -100, 100), n = n)
      fixture(paste0("pos_", tag), seed, runif(n, 0.01, 100), n = n)
      fixture(paste0("prob_", tag), seed, runif(n, 0.001, 0.999), n = n)
      fixture(paste0("na_", tag), seed, { x <- runif(n, -100, 100); x[sample.int(n, n %/% 10L)] <- NA; x }, n = n)
      fixture(paste0("sorted_", tag), seed, sort(runif(n, -100, 100)), n = n)
      fixture(paste0("int_", tag), seed, sample.int(1000L, n, replace = TRUE), n = n)
      fixture(paste0("lgl_", tag), seed, runif(n) < 0.5, n = n)
      fixture(paste0("chr_", tag), seed, sprintf("id%05d_%s", sample.int(99999L, n, replace = TRUE),
                                                 sample(letters, n, replace = TRUE)), n = n)
      fixture(paste0("txt_", tag), seed, sentence(n), n = n)
      fixture(paste0("fct_", tag), seed, factor(sample(letters, n, replace = TRUE)), n = n)
      fixture(paste0("cpl_", tag), seed, complex(real = runif(n, -1, 1), imaginary = runif(n, -1, 1)), n = n)
      fixture(paste0("raw_", tag), seed, as.raw(sample.int(256L, n, replace = TRUE) - 1L), n = n)
      fixture(paste0("lst_", tag), seed, as.list(runif(n, -100, 100)), n = n)
      fixture(paste0("named_", tag), seed, stats::setNames(runif(n, -100, 100), sprintf("n%07d", seq_len(n))), n = n)
      fixture(paste0("date_", tag), seed, as.Date("2000-01-01") + sample.int(10000L, n, replace = TRUE), n = n)
      fixture(paste0("time_", tag), seed,
              as.POSIXct("2000-01-01", tz = "UTC") + runif(n, 0, 3e8), n = n)
      fixture(paste0("datestr_", tag), seed,
              format(as.Date("2000-01-01") + sample.int(10000L, n, replace = TRUE)), n = n)
      fixture(paste0("numstr_", tag), seed, as.character(round(runif(n, -100, 100), 3)), n = n)
      fixture(paste0("df_", tag), seed, data.frame(
        id = seq_len(n), x = runif(n, -100, 100), y = runif(n, -100, 100),
        g = factor(sample(letters[1:10], n, replace = TRUE)),
        s = sprintf("id%05d", sample.int(99999L, n, replace = TRUE)), b = runif(n) < 0.5,
        stringsAsFactors = FALSE), n = n)
    })
  }
  fixture("dbl_1", 3L, 0.7341)
  fixture("int_1", 3L, 7L)
  fixture("lgl_1", 3L, TRUE)
  fixture("chr_1", 3L, "id00042_x")
  fixture("txt_1", 3L, "the price of the bond and the yield")
  fixture("cpl_1", 3L, complex(real = 0.5, imaginary = -0.25))
  fixture("date_1", 3L, as.Date("2021-06-15"))
  fixture("time_1", 3L, as.POSIXct("2021-06-15 12:34:56", tz = "UTC"))
  fixture("lst_10", 3L, list(a = 1, b = "x", c = TRUE, d = 1:3, e = NULL, f = list(1, 2), g = 2.5, h = "y", i = 3L, j = NA))
  fixture("mat_10", 4L, matrix(runif(100L, -1, 1), 10L, 10L))
  fixture("mat_100", 4L, matrix(runif(1e4, -1, 1), 100L, 100L))
  fixture("mat_1000", 4L, matrix(runif(1e6, -1, 1), 1000L, 1000L))
  fixture("imat_100", 4L, matrix(sample.int(100L, 1e4, replace = TRUE), 100L, 100L))
  fixture("spd_100", 4L, { m <- matrix(runif(1e4, -1, 1), 100L, 100L); crossprod(m) + diag(100L) })
  fixture("df_10", 4L, data.frame(id = 1:10, x = runif(10L), g = factor(letters[c(1:5, 1:5)]),
                                  s = letters[1:10], stringsAsFactors = FALSE))
  fixture("env_1e3", 5L, list2env(stats::setNames(as.list(seq_len(1000L)), sprintf("v%04d", seq_len(1000L)))))
  fixture("fun", 5L, function(x) x + 1)
  fixture("formula_1", 5L, y ~ x + g)
  fixture("ts_1e3", 5L, stats::ts(cumsum(rnorm(1000L)), frequency = 12, start = c(1990, 1)))
  fixture("model_1e3", 6L, {
    x <- runif(1000L)
    g <- factor(sample(letters[1:4], 1000L, replace = TRUE))
    data.frame(y = 2 * x + as.integer(g) + rnorm(1000L), x = x, g = g)
  })
  fixture_names <- ls(fixtures, all.names = TRUE)

  # The cases, read from the case files with `bench` and `skip` defined.
  cases <- list()
  skips <- list()
  unknown <- character()
  broken <- character()
  # Which exports are functions is asked only when listing: on ry, `get` forces each lazily
  # loaded function of base, which a timing process has no need for.
  exported <- if (mode == "list") lapply(stats::setNames(nm = packages), function(package) {
    names <- if (package == "base") ls(baseenv(), all.names = TRUE) else getNamespaceExports(package)
    where <- if (package == "base") baseenv() else asNamespace(package)
    sort(names[vapply(names, function(name) is.function(get(name, envir = where)), NA)])
  })
  known <- function(package, name) is.null(exported) || name %in% exported[[package]]
  one_line <- function(text) gsub("[[:space:]]+", " ", paste(text, collapse = " "))
  for (file in sort(list.files(directory, pattern = "\\.R$", full.names = TRUE))) {
    package <- sub("-.*$", "", sub("\\.R$", "", basename(file)))
    if (!package %in% packages) stop(file, " is not named after base, stats or utils")
    reader <- new.env(parent = baseenv())
    reader$bench <- function(.name, ..., setup = NULL) {
      # `.name` cannot be a prefix of a variant name, so a variant such as `n` or `na` is never taken for it.
      name <- .name
      expressions <- as.list(substitute(list(...)))[-1L]
      variants <- names(expressions)
      if (!length(expressions) || is.null(variants) || any(!grepl("^[a-z0-9_]+$", variants)) || anyDuplicated(variants)) {
        stop(file, ": bench(\"", name, "\") needs uniquely named cases, each name in lower case, digits and _")
      }
      if (!known(package, name)) unknown <<- c(unknown, paste0(package, "::", name))
      for (variant in variants) {
        id <- sprintf("%s::%s/%s", package, name, variant)
        if (!is.null(cases[[id]])) stop(file, ": ", id, " is defined twice")
        cases[[id]] <<- list(id = id, package = package, name = name, variant = variant,
                             expression = expressions[[variant]], setup = substitute(setup))
      }
      invisible()
    }
    reader$skip <- function(name, reason) {
      if (!known(package, name)) unknown <<- c(unknown, paste0(package, "::", name))
      skips[[paste0(package, "::", name)]] <<- list(package = package, name = name, reason = reason)
      invisible()
    }
    # A file that does not parse or run is reported and left out, so the others still list and run.
    tryCatch(for (expression in parse(file, keep.source = FALSE)) eval(expression, reader),
             error = function(condition) broken <<- c(broken, paste0(basename(file), "\t", one_line(conditionMessage(condition)))))
  }

  emit <- function(...) cat(..., "\n", file = out, append = TRUE, sep = "")

  if (mode == "list") {
    for (case in cases) {
      emit("case\t", case$id, "\t", case$package, "\t", case$name, "\t", case$variant, "\t",
           one_line(deparse(case$expression, width.cutoff = 500L)))
    }
    for (skipped in skips) emit("skip\t", skipped$package, "\t", skipped$name, "\t", one_line(skipped$reason))
    covered <- unique(c(vapply(cases, function(case) paste0(case$package, "::", case$name), ""), names(skips)))
    for (package in packages) {
      for (name in exported[[package]]) {
        if (!paste0(package, "::", name) %in% covered) emit("missing\t", package, "\t", name)
      }
    }
    for (name in unique(unknown)) emit("unknown\t", name)
    for (problem in broken) emit("broken\t", problem)
    return(invisible())
  }

  now <- function() as.numeric(Sys.time())
  wanted <- readLines(Sys.getenv("RY_SWEEP_CASES"))
  home <- tempfile("sweep-")
  dir.create(home)
  sinks <- sink.number()
  large_names <- grep("_1e6$|^mat_1000$", fixture_names, value = TRUE)
  built_large <- character()
  for (id in wanted) {
    case <- cases[[id]]
    if (is.null(case)) {
      emit(id, "\tfailed\tno such case")
      next
    }
    options_before <- options()
    setwd(home)
    line <- tryCatch({
      needed <- intersect(c(all.names(case$expression), all.names(case$setup)), fixture_names)
      dropped <- setdiff(built_large, needed)
      for (name in dropped) {
        rm(list = name, envir = fixtures)
        eval(recipes[[name]])
      }
      if (length(dropped) > 0L) invisible(gc())
      built_large <- intersect(union(built_large, needed), large_names)
      for (name in needed) get(name, envir = fixtures)
      scope <- new.env(parent = fixtures)
      set.seed(42L)
      if (!is.null(case$setup)) eval(case$setup, scope)
      # The case's calls run in a loop inside a closure, which GNU R compiles at its first call
      # and ry's JIT may compile once the loop is hot. The result of the last call is kept, so
      # neither engine can drop the calls as dead code.
      body <- bquote({
        result <- NULL
        for (iteration in seq_len(calls)) result <- .(case$expression)
        result
      })
      timer <- eval(call("function", as.pairlist(alist(calls = )), body), scope)
      sink(nullfile())
      warned <- NULL
      started <- now()
      result <- withCallingHandlers(timer(1), warning = function(condition) {
        warned <<- conditionMessage(condition)
        invokeRestart("muffleWarning")
      })
      first <- now() - started
      if (!is.null(warned)) stop("the call warns: ", warned)
      checksum <- .hot$checksum(result)
      result <- NULL
      invisible(gc(full = FALSE))
      if (first > slow_call) {
        times <- first
        calls <- 1
      } else {
        clock <- function(calls) {
          started <- now()
          timer(calls)
          now() - started
        }
        calls <- 1
        elapsed <- clock(calls)
        while (elapsed < batch_seconds && calls < 1e8) {
          calls <- min(1e8, max(calls * 2, ceiling(calls * 1.2 * batch_seconds / max(elapsed, 1e-6))))
          elapsed <- clock(calls)
        }
        times <- vapply(seq_len(batches), function(batch) clock(calls), 0) / calls
      }
      sprintf("%s\t%s\t%.4e\t%.4e\t%.0f", id, checksum, min(times), stats::median(times), calls)
    }, error = function(condition) {
      paste0(id, "\tfailed\t", one_line(conditionMessage(condition)))
    }, finally = {
      while (sink.number() > sinks) sink()
    })
    options(options_before)
    emit(line)
    invisible(gc(full = FALSE))
  }
})
