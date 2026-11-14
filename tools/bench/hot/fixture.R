# Writes a fake capture of the three code bases, in exactly the format ry's recorder writes, so
# the hot suite's tooling can be exercised without the recorder or the code bases:
#
#   Rscript tools/bench/hot/fixture.R DIR     (with R_LIBS on the corpus library, for data.table and zoo)
#
# then `hot.py validate --captures DIR`, `hot.py select --captures DIR --suite ...`, and so on.
# DIR/<code base>/ gets entries.tsv, samples.tsv, summary.tsv and samples/<n>.rds. The samples
# cover each case the validator and the harness must handle: a plain numeric call, an inline
# lambda, an imported name, a call that draws random numbers, a call that throws, one that
# prints, one whose result varies from call to call, one that needs a package from the corpus library, an
# oversized value the capture did not keep, an environment for a result, an operator, a slow
# call, a call that writes a file and a call whose kept value disagrees with its replay. A key
# with uncapturable time and no samples stands for what the recorder cannot replay.
local({
  out <- commandArgs(trailingOnly = TRUE)[[1L]]
  suppressPackageStartupMessages({
    loadNamespace("data.table")
    loadNamespace("zoo")
  })
  set.seed(7)
  numbers <- function(n) round(runif(n, -1, 1), 6)
  table_parts <- lapply(1:4, function(i) data.table::data.table(id = seq_len(50) + 50L * i, value = numbers(50)))
  rnorm_seed <- {
    set.seed(42)
    get(".Random.seed", envir = globalenv())
  }

  # Each sample: the key, the call, its variables and imports, and the code bases that made it.
  sample <- function(package, name, call, variables = list(), imports = character(), seed = NULL,
                     keep = TRUE, threw = FALSE, wrong = FALSE, codebases) {
    list(package = package, name = name, call = call, variables = variables, imports = imports,
         seed = seed, keep = keep, threw = threw, wrong = wrong, codebases = codebases)
  }
  all <- c("finbench", "dsbench", "biobench")
  samples <- list(
    sample("stats", "sd", quote(stats::sd(x)), list(x = numbers(50)), codebases = all),
    sample("stats", "sd", quote(stats::sd(x)), list(x = numbers(400)), codebases = all),
    sample("stats", "sd", quote(sd(x)), list(x = numbers(5000)), imports = c(sd = "stats"), codebases = "finbench"),
    sample("base", "vapply", quote(vapply(xs, function(v) v * k, numeric(1))), list(xs = as.list(numbers(100)), k = 3),
           codebases = all),
    sample("zoo", "rollmean", quote(rollmean(z, 3)), list(z = cumsum(numbers(60))), imports = c(rollmean = "zoo"),
           codebases = c("finbench", "biobench")),
    sample("stats", "rnorm", quote(stats::rnorm(n)), list(n = 100L), seed = rnorm_seed, codebases = c("finbench", "biobench")),
    sample("base", "match.arg", quote(match.arg(type, c("linear", "log"))), list(type = "cubic"), keep = FALSE,
           threw = TRUE, codebases = "finbench"),
    sample("base", "print", quote(print(x)), list(x = 1:100), codebases = "dsbench"),
    sample("base", "Sys.time", quote(Sys.time()), codebases = "dsbench"),
    sample("data.table", "rbindlist", quote(data.table::rbindlist(parts)), list(parts = table_parts),
           codebases = c("dsbench", "finbench")),
    sample("base", "cumsum", quote(cumsum(big)), list(big = numbers(200000)), keep = FALSE, codebases = c("finbench", "biobench")),
    sample("base", "list2env", quote(list2env(items)), list(items = list(a = 1, b = "two")), codebases = c("dsbench", "biobench")),
    sample("base", "paste0", quote(paste0(prefix, ids)), list(prefix = "trade-", ids = 1:200), codebases = c("finbench", "dsbench")),
    sample("base", "[", quote(x[idx]), list(x = numbers(1000), idx = seq(1L, 1000L, by = 7L)), codebases = c("finbench", "dsbench")),
    sample("base", "Sys.sleep", quote(Sys.sleep(0.3)), keep = FALSE, codebases = "biobench"),
    sample("base", "writeLines", quote(writeLines(text, tempfile())), list(text = c("a", "b")), keep = FALSE, codebases = "dsbench"),
    sample("stats", "median", quote(stats::median(x)), list(x = numbers(301)), codebases = c("finbench", "biobench")),
    sample("stats", "median", quote(stats::median(x)), list(x = numbers(301)), wrong = TRUE, codebases = "biobench")
  )

  # Calls per key and code base, with the mean seconds a call takes, so the shares differ between
  # code bases. The uncapturable `purrr::map` time comes from calls passing `...` along.
  calls <- list(
    "stats sd" = c(finbench = 40000, dsbench = 5000, biobench = 30000),
    "base vapply" = c(finbench = 9000, dsbench = 20000, biobench = 3000),
    "zoo rollmean" = c(finbench = 12000, biobench = 2000),
    "stats rnorm" = c(finbench = 20000, biobench = 25000),
    "base match.arg" = c(finbench = 60000),
    "base print" = c(dsbench = 3000),
    "base Sys.time" = c(dsbench = 9000),
    "data.table rbindlist" = c(dsbench = 6000, finbench = 1000),
    "base cumsum" = c(finbench = 500, biobench = 900),
    "base list2env" = c(dsbench = 8000, biobench = 1000),
    "base paste0" = c(finbench = 15000, dsbench = 30000),
    "base [" = c(finbench = 100000, dsbench = 50000),
    "base Sys.sleep" = c(biobench = 2),
    "base writeLines" = c(dsbench = 400),
    "stats median" = c(finbench = 7000, biobench = 16000)
  )
  per_call <- c("stats sd" = 2e-5, "base vapply" = 6e-5, "zoo rollmean" = 1e-4, "stats rnorm" = 8e-6,
                "base match.arg" = 3e-6, "base print" = 1e-4, "base Sys.time" = 2e-6, "data.table rbindlist" = 2e-4,
                "base cumsum" = 9e-4, "base list2env" = 4e-6, "base paste0" = 3e-5, "base [" = 2e-6,
                "base Sys.sleep" = 0.3, "base writeLines" = 5e-5, "stats median" = 2e-5)

  for (codebase in all) {
    directory <- file.path(out, codebase)
    unlink(directory, recursive = TRUE)
    dir.create(file.path(directory, "samples"), recursive = TRUE)
    rows <- list()
    number <- 0L
    for (spec in samples) {
      if (!codebase %in% spec$codebases) next
      number <- number + 1L
      env <- list2env(spec$variables, parent = globalenv())
      for (name in names(spec$imports)) assign(name, get(name, envir = asNamespace(spec$imports[[name]])), envir = env)
      if (!is.null(spec$seed)) assign(".Random.seed", spec$seed, envir = globalenv())
      value <- NULL
      if (spec$keep) {
        value <- withCallingHandlers(capture.output(result <- eval(spec$call, env)), warning = function(w) invokeRestart("muffleWarning"))
        value <- if (spec$wrong) result + 1 else result
      }
      key <- paste(spec$package, spec$name)
      seconds <- per_call[[key]] * (0.8 + 0.1 * number %% 5)
      file <- sprintf("samples/%d.rds", number)
      saveRDS(list(package = spec$package, name = spec$name, call = spec$call, variables = spec$variables,
                   imports = spec$imports, seed = spec$seed, value = value, value_kept = spec$keep,
                   threw = spec$threw, seconds = seconds), file.path(directory, file))
      rows[[length(rows) + 1L]] <- data.frame(file = file, package = spec$package, name = spec$name,
                                              seconds = seconds, threw = spec$threw,
                                              bytes = file.size(file.path(directory, file)))
    }
    write.table(do.call(rbind, rows), file.path(directory, "samples.tsv"), sep = "\t", quote = FALSE, row.names = FALSE)

    entries <- list()
    for (key in names(calls)) {
      count <- calls[[key]][codebase]
      if (is.na(count)) next
      parts <- strsplit(key, " ", fixed = TRUE)[[1L]]
      sampled <- sum(vapply(rows, function(row) row$package == parts[[1L]] && row$name == parts[[2L]], TRUE))
      entries[[length(entries) + 1L]] <- data.frame(package = parts[[1L]], name = parts[[2L]], calls = count,
        seconds = count * per_call[[key]], samples = sampled, uncapturable_calls = 0, uncapturable_seconds = 0,
        dots = 0, local_closure = 0, user_binding = 0, oversize = if (key == "base cumsum") 3 else 0)
    }
    entries[[length(entries) + 1L]] <- data.frame(package = "purrr", name = "map", calls = 0, seconds = 0, samples = 0,
      uncapturable_calls = 4000, uncapturable_seconds = 1.5, dots = 3000, local_closure = 1000, user_binding = 0, oversize = 0)
    entries <- do.call(rbind, entries)
    write.table(entries, file.path(directory, "entries.tsv"), sep = "\t", quote = FALSE, row.names = FALSE)

    covered <- sum(entries$seconds)
    uncapturable <- sum(entries$uncapturable_seconds)
    user <- covered * 0.8
    outside <- covered * 0.3
    summary <- c(total_seconds = user + covered + uncapturable + outside, user_seconds = user, covered_seconds = covered,
                 uncapturable_seconds = uncapturable, outside_seconds = outside,
                 boundary_calls = sum(entries$calls) + sum(entries$uncapturable_calls), keys = nrow(entries))
    writeLines(c("key\tvalue", sprintf("%s\t%.6f", names(summary), summary)), file.path(directory, "summary.tsv"))
  }
})
