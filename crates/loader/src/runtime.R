# ry's package loader, R side, shared by both runtimes. It applies plans that ry computes from
# its analysis of the package: every function definition, and every pure constant of a strict
# file, becomes a promise that evaluates only that definition on first use, and other top-level
# code runs in collation order. A reload
# applies a plan that touches only the items that changed.
#
# A backend file follows this one and defines how code reaches R: `gnur.R` parses source text in
# R, and `ry.R` hands ry's interpreter the analysis's syntax trees. The backend's
# `.ry_await_verdict()` returns once the host lets the package's effects run. The host defines the
# transport, `.ry_request(root, full, conditions)` (which returns the plan, given the values of
# the conditions of `if` directives in NAMESPACE, see `.ry_namespace_conditions`) and
# `.ry_commit(root)`, and
# creates the session's state, `.ry_packages` (one entry per loaded package root) and
# `.ry_timing`, since a host may cache the compiled functions across sessions and state must
# not be.

# The values of the conditions of each `if` directive (given as source text), encoded for
# `.ry_request`: per directive, a line of `T` and `F` letters, a tab, and the escaped text. As in
# `parseNamespaceFile`, a condition is evaluated in the global environment, and only the
# conditions on the branch taken are, depth first.
.ry_namespace_conditions <- function(directives) {
  lines <- vapply(directives, function(text) {
    values <- logical()
    walk <- function(e) {
      if (!is.call(e)) return(invisible())
      head <- as.character(e[[1L]])[[1L]]
      if (identical(head, "if")) {
        value <- if (eval(e[[2L]], .GlobalEnv)) TRUE else FALSE
        values <<- c(values, value)
        if (value) walk(e[[3L]]) else if (length(e) == 4L) walk(e[[4L]])
      } else if (identical(head, "{")) {
        for (inner in as.list(e)[-1L]) walk(inner)
      }
    }
    walk(parse(text = text, keep.source = FALSE, encoding = "UTF-8")[[1L]])
    escaped <- gsub("\r", "\\r", gsub("\n", "\\n", gsub("\\", "\\\\", text, fixed = TRUE), fixed = TRUE), fixed = TRUE)
    paste0(paste(ifelse(values, "T", "F"), collapse = ""), "\t", escaped)
  }, character(1), USE.NAMES = FALSE)
  paste(lines, collapse = "\n")
}

.ry_stoplist <- c(
  ".__NAMESPACE__.", ".__S3MethodsTable__.", ".packageName", ".First.lib", ".onLoad",
  ".onAttach", ".onUnload", ".conflicts.OK", ".noGenerics", ".__ry_loader__."
)

load_all <- function(path = ".", export_all = TRUE, attach = TRUE, full = FALSE, quiet = FALSE) {
  .ry_load(path, export_all, attach, full, quiet, report_noop = TRUE)
}

.ry_load <- function(path, export_all, attach, full, quiet, report_noop) {
  root <- .ry_find_root(path)
  state <- .ry_packages[[root]]
  current <- !is.null(state) && isNamespaceLoaded(state$name) &&
    identical(asNamespace(state$name), state$ns)
  full <- isTRUE(full) || !current
  started <- proc.time()[["elapsed"]]
  .ry_phase(NULL)
  # R evaluates the conditions of NAMESPACE's `if` directives at every load, and only those of
  # the directives the file holds now, so the first request sends no values and the plan names
  # the directives whose conditions it needs. A file edited between two requests names new
  # ones, and the same values twice means the plan cannot use them.
  conditions <- ""
  repeat {
    plan <- .ry_request(root, full, conditions)
    if (!isTRUE(plan$unresolved)) break
    evaluated <- .ry_namespace_conditions(plan$namespace_conditionals)
    if (identical(evaluated, conditions)) {
      stop("the `if` directives in NAMESPACE could not be resolved", call. = FALSE)
    }
    conditions <- evaluated
  }
  .ry_phase("plan")
  if (!is.null(plan$error)) {
    stop(plan$error, call. = FALSE)
  }
  if (length(plan$warnings)) {
    for (warning_text in plan$warnings) warning(warning_text, call. = FALSE)
  }
  # As `pkgload`, a package with testthat tests gets testthat attached and, when the load
  # exports everything onto the search path, its helpers sourced there.
  testthat <- .ry_uses_testthat(root)
  attached <- isTRUE(attach) && (plan$full || !is.null(state$package_env))
  helpers <- .ry_helpers(state, root, isTRUE(export_all) && attached && testthat,
                         plan$full || !plan$noop)
  # Switching `export_all` changes what is exported even when no file changed.
  resync <- !identical(state$export_all, export_all)
  if (!plan$full && plan$noop && current && !resync && is.null(helpers)) {
    state$changed <- character()
    .ry_commit(root)
    if (!quiet && report_noop) {
      message(sprintf("%s is up to date.", plan$package))
    }
    return(invisible(state$ns))
  }
  if (plan$full) {
    state <- .ry_new_package(plan, root)
  } else if (.ry_reaches_deferred_s4(state, plan)) {
    .ry_force_s4(state, all = TRUE)
  }
  state$export_all <- export_all
  state$attach_testthat <- testthat && plan$package != "testthat"
  state$namespace_spec <- plan$namespace
  # Exports are resynced by scanning every name in the namespace, which a reload that only
  # rebinds existing names cannot change, unless it runs `.onLoad`, which may bind any name.
  plan$keeps_names <- isTRUE(plan$keeps_names) && !resync && !isTRUE(plan$run_on_load)
  if (!plan$full) {
    .ry_refuse_missing_s3(state, plan)
  }
  # A statement that fails leaves the namespace partly updated, so the attached package
  # environment follows it either way rather than mixing two versions.
  applied <- FALSE
  on.exit(if (!applied && !is.null(state$package_env)) .ry_sync_package_env(state), add = TRUE)
  .ry_apply(plan, state)
  applied <- TRUE
  # The names this load rebound, removed or changed the S3 dispatch of, which `ry test` reruns
  # the tests of first. `NULL` when the load ran code, which may have changed anything.
  state$changed <- if (plan$full) NULL else plan$changed
  # The old helpers' names go before the sync, which then restores an export one of them hid.
  if (!is.null(helpers)) {
    .ry_drop_helpers(state)
  }
  if (plan$full && isTRUE(attach)) {
    .ry_attach(plan, state)
  } else if (!is.null(state$package_env)) {
    .ry_sync_package_env(state, if (resync) NULL else plan$touched, plan$keeps_names)
  }
  .ry_phase("attach")
  if (!is.null(helpers) && !is.null(state$package_env)) {
    .ry_source_helpers(state, helpers)
  }
  .ry_commit(root)
  .ry_update_importers(state)
  if (!quiet) {
    elapsed <- proc.time()[["elapsed"]] - started
    held <- if (is.null(state$s4_deferred)) 0L else length(state$s4_deferred$run)
    message(.ry_summary(plan, elapsed, held, helpers = !is.null(helpers)))
  }
  invisible(state$ns)
}

# A reload that would leave a declared S3 method without its function is refused before it
# changes anything, as R refuses such a package. The new `sysdata.rda` is read into a scratch
# environment here, so its names count. Only a statement that may write a name it computes
# (`run_writes` is `NULL`) can still define the function, and then `.ry_register_s3` checks
# after the statements ran instead.
.ry_refuse_missing_s3 <- function(state, plan) {
  sysdata <- character()
  state$sysdata_pending <- NULL
  if (identical(plan$sysdata, "load")) {
    state$sysdata_pending <- new.env(parent = emptyenv())
    sysdata <- load(file.path(state$root, "R", "sysdata.rda"), envir = state$sysdata_pending)
  }
  if (is.null(plan$run_writes)) {
    return(invisible())
  }
  gone <- c(plan$remove, if (!identical(plan$sysdata, "keep")) state$sysdata)
  for (method in plan$s3_register) {
    name <- method$`function`
    bound <- name %in% plan$bind_name || name %in% plan$run_writes || name %in% sysdata ||
      (!(name %in% gone) && exists(name, envir = state$ns, inherits = FALSE))
    if (!bound) {
      stop(sprintf("object '%s' not found whilst loading namespace '%s'", name, state$name),
           call. = FALSE)
    }
  }
}

# Whether the package has testthat tests, as `pkgload` decides it: it has a `tests/testthat` or
# an `inst/tests` directory, and testthat is installed.
.ry_uses_testthat <- function(root) {
  any(dir.exists(file.path(root, c("inst/tests", "tests/testthat")))) &&
    requireNamespace("testthat", quietly = TRUE)
}

# The testthat helpers, the `helper*.R` files of `tests/testthat` (else of `inst/tests`), which
# `load_all()` sources into the attached package environment when it exports everything there,
# as `pkgload` does, so tests and the console can call them but the namespace does not hold them.
# A helper may compute a value from the package's code, so a reload sources them again when the
# load changed anything or the helpers' names, sizes or modification times changed, and returns
# NULL otherwise.
.ry_helpers <- function(state, root, enabled, rerun) {
  directory <- file.path(root, "tests", "testthat")
  if (!dir.exists(directory)) directory <- file.path(root, "inst", "tests")
  files <- if (enabled) {
    sort(list.files(directory, pattern = "^helper.*\\.[rR]$", full.names = TRUE))
  } else {
    character()
  }
  if (!length(files)) {
    return(if (length(state$helper_names)) list(files = character(), stamp = NULL))
  }
  info <- file.info(files, extra_cols = FALSE)
  stamp <- paste(files, info$size, as.numeric(info$mtime), collapse = "\n")
  if (!rerun && identical(state$helpers_stamp, stamp)) {
    return(NULL)
  }
  list(files = files, stamp = stamp)
}

# Names a helper bound are removed before the helpers run again, so a deleted helper leaves
# nothing behind.
.ry_drop_helpers <- function(state) {
  environment <- state$package_env
  if (!is.null(environment) && length(state$helper_names)) {
    rm(list = intersect(state$helper_names, names(environment)), envir = environment)
  }
  state$helper_names <- NULL
  state$helpers_stamp <- NULL
}

# The helpers run with `NOT_CRAN` set, as `pkgload` runs them. The stamp is recorded only once
# every helper ran, so a helper that failed runs again on the next load.
.ry_source_helpers <- function(state, helpers) {
  environment <- state$package_env
  before <- names(environment)
  not_cran <- Sys.getenv("NOT_CRAN", unset = NA)
  Sys.setenv(NOT_CRAN = "true")
  on.exit({
    state$helper_names <- setdiff(names(environment), before)
    if (is.na(not_cran)) Sys.unsetenv("NOT_CRAN") else Sys.setenv(NOT_CRAN = not_cran)
  })
  keep <- isTRUE(getOption("keep.source", interactive()))
  for (file in helpers$files) {
    withCallingHandlers(
      sys.source(file, envir = environment, chdir = TRUE, keep.source = keep),
      error = function(error) {
        message(sprintf("Error while loading %s", file))
      }
    )
  }
  state$helpers_stamp <- helpers$stamp
}

# With `options(ry.load.verbose = TRUE)`, or `RY_LOAD_VERBOSE=true` in the environment for a
# load that starts before any code could set the option, reports how long each phase of a load
# took. The first load of a process also reports the time the process took to reach it.
.ry_phase <- function(label) {
  now <- proc.time()[["elapsed"]]
  if (.ry_verbose()) {
    if (!is.null(label)) {
      message(sprintf("  %-14s %7.1f ms", label, (now - .ry_timing$last) * 1000))
    } else if (is.null(.ry_timing$reported_start)) {
      message(sprintf("  %-14s %7.1f ms", "process start", now * 1000))
    }
  }
  .ry_timing$reported_start <- TRUE
  .ry_timing$last <- now
}

.ry_verbose <- function() {
  isTRUE(getOption("ry.load.verbose", as.logical(Sys.getenv("RY_LOAD_VERBOSE", "false"))))
}

# Reloads the packages whose sources changed, including one whose load failed, which has no
# state yet. A console calls it before a command when the host sees a change on disk, and it
# keeps `.Last.value`.
.ry_autoreload <- function(roots = .ry_changed_roots()) {
  last <- .Last.value
  for (root in roots) {
    export_all <- .ry_packages[[root]]$export_all
    tryCatch(
      .ry_load(root, if (is.null(export_all)) TRUE else export_all, attach = TRUE,
               full = FALSE, quiet = FALSE, report_noop = FALSE),
      error = function(error) {
        message("ry: reloading ", root, " failed: ", conditionMessage(error))
      }
    )
  }
  invisible(last)
}

# `system.file()` as `pkgload` shims it, for a package loaded from source: a path is looked up in
# `inst/` first and then at the root, because installing moves the contents of `inst/` to the
# top. Package code reaches this through the namespace's imports and other code through
# `tools:ry`; any other package gets base R's.
system.file <- function(..., package = "base", lib.loc = NULL, mustWork = FALSE) {
  root <- if (is.null(lib.loc)) .ry_source_root(package)
  if (is.null(root)) {
    return(base::system.file(..., package = package, lib.loc = lib.loc, mustWork = mustWork))
  }
  if (!...length()) {
    return(root)
  }
  inst <- file.path(root, "inst", ...)
  top <- file.path(root, ...)
  in_inst <- file.exists(inst)
  files <- ifelse(in_inst, inst, top)[in_inst | file.exists(top)]
  if (length(files)) {
    sub("/$", "", normalizePath(files, winslash = "/"))
  } else if (mustWork) {
    stop("no file found", call. = FALSE)
  } else {
    ""
  }
}

# The root of the package named `package` when the loaded namespace of that name is one ry
# loaded from source.
.ry_source_root <- function(package) {
  if (!is.character(package) || length(package) != 1L || !isNamespaceLoaded(package)) {
    return(NULL)
  }
  ns <- asNamespace(package)
  for (root in ls(.ry_packages, all.names = TRUE)) {
    if (identical(.ry_packages[[root]]$ns, ns)) return(root)
  }
  NULL
}

# A lazy `View()`: the editor connected to the console pages through `x` and formats only the
# rows it shows, where utils' version formats the whole object first.
View <- function(x, title) {
  if (missing(title)) title <- paste("Data:", deparse(substitute(x))[1])
  invisible(.ry_view(x, title))
}

.ry_find_root <- function(path) {
  path <- normalizePath(path, winslash = "/", mustWork = TRUE)
  current <- path
  repeat {
    if (file.exists(file.path(current, "DESCRIPTION"))) {
      return(current)
    }
    parent <- dirname(current)
    if (identical(parent, current)) {
      stop(path, " is not inside an R package: no DESCRIPTION file was found in it or above it",
           call. = FALSE)
    }
    current <- parent
  }
}

.ry_internal <- function(name, ...) {
  arguments <- lapply(list(...), function(argument) {
    if (is.language(argument)) call("quote", argument) else argument
  })
  eval(as.call(list(quote(.Internal), as.call(c(list(as.name(name)), arguments)))))
}

.ry_new_package <- function(plan, root) {
  name <- plan$package
  previous <- .ry_packages[[root]]
  if (!is.null(previous)) {
    .ry_drop_deferred_s4(previous)
  }
  .ry_teardown(name)
  for (package in plan$depends) {
    if (!(paste0("package:", package) %in% search())) {
      library(package, character.only = TRUE, warn.conflicts = FALSE)
    }
  }
  imports <- new.env(parent = .BaseNamespaceEnv, hash = TRUE)
  attr(imports, "name") <- paste0("imports:", name)
  ns <- new.env(parent = imports, hash = TRUE)
  info <- new.env(hash = TRUE, parent = baseenv())
  ns[[".__NAMESPACE__."]] <- info
  info[["spec"]] <- c(name = name, version = plan$version)
  setNamespaceInfo(ns, "exports", new.env(hash = TRUE, parent = baseenv()))
  lazydata <- new.env(parent = baseenv(), hash = TRUE)
  attr(lazydata, "name") <- paste0("lazydata:", name)
  setNamespaceInfo(ns, "lazydata", lazydata)
  setNamespaceInfo(ns, "imports", list(base = TRUE))
  setNamespaceInfo(ns, "path", root)
  setNamespaceInfo(ns, "dynlibs", NULL)
  setNamespaceInfo(ns, "nativeRoutines", list())
  setNamespaceInfo(ns, "S3methods", matrix(NA_character_, 0L, 4L))
  ns[[".__S3MethodsTable__."]] <- new.env(hash = TRUE, parent = baseenv())
  ns[[".packageName"]] <- name
  ns[[".__ry_loader__."]] <- TRUE
  .ry_internal("registerNamespace", name, ns)
  if (isNamespaceLoaded("methods")) {
    methods::setPackageName(name, ns)
  }
  state <- new.env(parent = emptyenv())
  state$name <- name
  state$root <- root
  state$ns <- ns
  state$sysdata <- character()
  state$dlls <- list()
  state$package_env <- NULL
  assign(root, state, envir = .ry_packages)
  state
}

# Another package loaded from source that imports this one holds copies of its bindings, which
# this load replaced, so its imports are rebuilt from its own `NAMESPACE` directives, and its
# attached environment follows, since it may re-export an import. This load has already
# succeeded, so an importer whose directives no longer fit (it imports a name this load
# removed) keeps its old imports and gets a warning rather than failing the load.
.ry_update_importers <- function(state) {
  for (root in ls(.ry_packages, all.names = TRUE)) {
    importer <- .ry_packages[[root]]
    if (identical(importer$name, state$name) || is.null(importer$import_directives) ||
        !isNamespaceLoaded(importer$name) ||
        !identical(asNamespace(importer$name), importer$ns) ||
        !(state$name %in% names(getNamespaceInfo(importer$ns, "imports")))) {
      next
    }
    rebuilt <- tryCatch({
      .ry_import(importer$ns, importer$name, importer$import_directives)
      TRUE
    }, error = function(error) {
      warning(sprintf("%s's imports could not be rebuilt, so it keeps the old ones: %s",
                      importer$name, conditionMessage(error)), call. = FALSE)
      FALSE
    })
    if (rebuilt && !is.null(importer$package_env)) {
      .ry_sync_package_env(importer)
    }
  }
}

# A namespace that other namespaces import cannot be unloaded, which is the normal case when
# several development packages import each other, so it is unregistered instead, and
# the load that follows rebuilds its importers' copies (see `.ry_update_importers`).
.ry_teardown <- function(name) {
  package <- paste0("package:", name)
  if (package %in% search()) {
    detach(package, character.only = TRUE, unload = FALSE, force = TRUE)
  }
  if (isNamespaceLoaded(name)) {
    unloaded <- tryCatch({
      unloadNamespace(name)
      TRUE
    }, error = function(error) FALSE)
    if (!unloaded && isNamespaceLoaded(name)) {
      .ry_internal("unregisterNamespace", name)
    }
  }
}

.ry_apply <- function(plan, state) {
  ns <- state$ns
  keep <- isTRUE(getOption("keep.source", interactive()))
  if (!is.null(plan$imports)) {
    .ry_import(ns, state$name, plan$imports)
    state$import_directives <- plan$imports
  }
  .ry_phase("imports")
  .ry_sources(state, plan)
  if (length(plan$remove)) {
    rm(list = intersect(plan$remove, ls(ns, all.names = TRUE)), envir = ns)
  }
  for (registration in plan$unregister) {
    .ry_unregister(ns, registration)
  }
  .ry_phase("sources")
  if (length(plan$bind_name)) {
    .ry_bind_lazily(state, plan, keep)
  }
  .ry_phase("bind lazily")
  if (plan$full && !is.null(plan$deferred_s4) && .ry_may_defer_s4(state, plan$deferred_s4)) {
    .ry_defer_s4(state, plan$deferred_s4)
  }
  if (length(plan$run_path)) {
    previous <- options(topLevelEnvironment = ns)
    on.exit(options(previous), add = TRUE)
    for (i in seq_along(plan$run_path)) {
      # Code that runs at load may reach a deferred class or generic, which runs the
      # definitions placed before it, and the rest then run here in their places.
      if (!is.null(state$s4_deferred) && i %in% state$s4_deferred$run) {
        next
      }
      state$s4_cursor <- i
      withCallingHandlers(
        .ry_run(state, plan, i, keep),
        error = function(error) {
          message(sprintf("Error while loading %s:%d", plan$run_path[[i]], plan$run_line[[i]]))
        }
      )
    }
  }
  state$s4_cursor <- NULL
  .ry_phase("run")
  # Everything above only binds names, registers classes and loads imported packages, unless a
  # statement the plan did not mark effect-free ran, and that one waited. `data/*.R`, compiled
  # code and hooks may have effects.
  .ry_await_verdict()
  if (!identical(plan$sysdata, "keep")) {
    if (length(state$sysdata)) {
      rm(list = intersect(state$sysdata, ls(ns, all.names = TRUE)), envir = ns)
    }
    state$sysdata <- if (!is.null(state$sysdata_pending)) {
      pending <- state$sysdata_pending
      state$sysdata_pending <- NULL
      list2env(as.list(pending, all.names = TRUE), envir = ns)
      ls(pending, all.names = TRUE, sorted = FALSE)
    } else if (identical(plan$sysdata, "load")) {
      load(file.path(state$root, "R", "sysdata.rda"), envir = ns)
    } else {
      character()
    }
  }
  if (!is.null(plan$data)) {
    .ry_load_data(state, plan$data)
  }
  # Each S4 definition caches what it defines as it runs, so this only refreshes what an
  # earlier load left behind. While definitions are held back it would read their placeholders
  # and run them, so it waits for them: running them ends with the same call.
  if (isNamespaceLoaded("methods") && (plan$full || isTRUE(plan$registers_s4)) &&
      is.null(state$s4_deferred) && .ry_has_s4_metadata(ns)) {
    methods::cacheMetaData(ns, TRUE, ns)
  }
  .ry_phase("S4 caches")
  for (method in plan$s3_unregister) {
    .ry_unregister_s3(ns, method)
  }
  for (method in plan$s3_register) {
    .ry_register_s3(ns, state$name, method)
  }
  if (!is.null(plan$s3_declared)) {
    declared <- plan$s3_declared
    matrix_rows <- lapply(declared, function(method) {
      c(method$generic, method$class, method$`function`, NA_character_)
    })
    setNamespaceInfo(ns, "S3methods", if (length(matrix_rows)) {
      do.call(rbind, matrix_rows)
    } else {
      matrix(NA_character_, 0L, 4L)
    })
  }
  if (!is.null(plan$dynamic_libraries)) {
    .ry_load_compiled(state, plan$dynamic_libraries)
  }
  .ry_phase("S3")
  # pkgload attaches testthat after the code and before `.onLoad`, so it lands on the search
  # path behind the package, which is attached later.
  if (isTRUE(state$attach_testthat)) {
    library("testthat", warn.conflicts = FALSE)
  }
  if (isTRUE(plan$run_on_load) && exists(".onLoad", envir = ns, inherits = FALSE)) {
    get(".onLoad", envir = ns, inherits = FALSE)(dirname(state$root), state$name)
  }
  # After `.onLoad`, as in pkgload, so a name it binds in the namespace is exported too.
  if (!plan$keeps_names) {
    .ry_sync_exports(ns, plan$namespace, state$export_all)
  }
  .ry_phase("exports")
}

# Reads the datasets of `data/` into the namespace's lazy data the way `data()` reads them, as
# pkgload does: `::` finds them there, and attaching copies them onto the search path.
.ry_load_data <- function(state, files) {
  lazydata <- getNamespaceInfo(state$ns, "lazydata")
  rm(list = ls(lazydata, all.names = TRUE), envir = lazydata)
  for (path in files) {
    file <- file.path(state$root, path)
    name <- sub("\\.[^.]*$", "", basename(path))
    table <- function(...) {
      assign(name, utils::read.table(file, header = TRUE, as.is = FALSE, ...), envir = lazydata)
    }
    switch(tolower(sub("^.*\\.", "", path)),
      r = {
        # `data()` runs a script in a child of the global environment, which reaches the
        # `system.file()` shim on `tools:ry`; `lazydata`'s parent is the base environment.
        scratch <- new.env(parent = globalenv())
        sys.source(file, envir = scratch, chdir = TRUE)
        for (object in ls(scratch, all.names = TRUE)) {
          assign(object, get(object, envir = scratch), envir = lazydata)
        }
      },
      rda = , rdata = load(file, envir = lazydata),
      csv = table(sep = ";"),
      tab = , txt = table()
    )
  }
}

# A fresh load may hold back the package's S4 definitions (`plan$deferred_s4`, which the
# loader's `DeferredS4` documents) until code first reaches one of them, in units: the first
# holds the classes and everything not split off, and each of `lazy_generics` holds one
# generic's definition and methods. Each name a unit binds is a promise that runs it, and so
# is looking up one of its classes or generics by name. A generic's unit runs the first unit
# before it.
.ry_defer_s4 <- function(state, deferred) {
  # A method for another package's generic is stored under that package's name, which only the
  # generic itself knows. Dispatch, `getMethod()` and `existsMethod()` find it under its label
  # in the tables in the generic's own environment, which they read without any lookup the
  # interpreter sees, so each label the held-back methods fill becomes a promise that runs
  # the first unit. Every other label stays as it is, so dispatch on other classes runs nothing.
  # A method for a plain function such as `as.list` makes a generic of it in the namespace, so
  # that name is a promise as well, and a primitive's methods take effect when it dispatches,
  # which the interpreter runs the first unit before when an argument has one of its classes.
  labels <- list()
  generics <- setdiff(deferred$generics, deferred$lazy_generics)
  primitives <- character()
  for (method in deferred$foreign_methods) {
    name <- method$generic
    generic <- get(name, envir = state$ns, mode = "function")
    kind <- .ry_foreign_generic_kind(generic)
    if (kind == "standard") {
      label <- .ry_s4_label(generic, method$signature)
      environment <- environment(generic)
      for (table in intersect(c(".MTable", ".AllMTable"), names(environment))) {
        table <- get(table, envir = environment)
        held <- exists(label, envir = table, inherits = FALSE)
        labels[[length(labels) + 1L]] <- list(
          table = table, label = label, held = held,
          value = if (held) get(label, envir = table, inherits = FALSE)
        )
      }
    }
    if (name %in% deferred$generics) {
      next
    }
    package <- switch(kind,
      standard = attr(generic, "package"),
      primitive = "base",
      implicit = methods::getPackageName(environment(generic))
    )
    deferred$names <- c(deferred$names, sprintf(".__T__%s:%s", name, package))
    if (kind == "primitive") {
      primitives <- c(primitives, name)
    } else if (kind == "implicit") {
      deferred$names <- c(deferred$names, name)
    }
    if (kind != "standard") {
      generics <- c(generics, name)
    }
  }
  deferred$names <- unique(deferred$names)
  # Which unit binds each name: the first unless a lazy generic's unit does.
  unit_of <- new.env(hash = TRUE, parent = emptyenv())
  for (k in seq_along(deferred$lazy_generics)) {
    for (name in deferred$lazy_names[[k]]) {
      assign(name, k, envir = unit_of)
    }
  }
  deferred$first_names <- deferred$names[!vapply(deferred$names, exists, NA,
    envir = unit_of, inherits = FALSE)]
  state$s4_deferred <- deferred
  state$s4_ran <- logical(length(deferred$lazy_generics) + 1L)
  state$s4_labels <- labels
  for (label in labels) {
    .ry_s4_label_placeholder(state, label)
  }
  force <- function(generic) .ry_force_s4(state, generic)
  .Internal(ryLoaderDefer(state$root, deferred$run, deferred$classes, generics, primitives,
    deferred$lazy_generics, force))
  for (name in deferred$names) {
    k <- get0(name, envir = unit_of, inherits = FALSE)
    .ry_s4_placeholder(state, name, if (!is.null(k)) deferred$lazy_generics[[k]])
  }
}

# Another package's `setMethod()` on the same generic reads every label, so a force can come
# during this load from a method placed after the statement now running, which has not run.
.ry_s4_label_placeholder <- function(state, label) {
  delayedAssign(label$label, {
    .Internal(ryLoaderForceUnit(state$root, NULL))
    get0(label$label, envir = label$table, inherits = FALSE)
  }, assign.env = label$table)
}

# Puts back what the method tables of other packages' generics held under the labels the
# held-back definitions replaced with promises.
.ry_restore_s4_labels <- function(state) {
  for (label in state$s4_labels) {
    if (label$held) {
      assign(label$label, label$value, envir = label$table)
    } else if (exists(label$label, envir = label$table, inherits = FALSE)) {
      # Two held-back methods can share a label, as `"A"` and `c("A", "ANY")` do.
      rm(list = label$label, envir = label$table)
    }
  }
  state$s4_labels <- NULL
}

# A force during the load runs only the definitions before the statement now running, so a
# name a later one binds is not there yet, and the statement sees what an eager load shows it
# at that point: the imported or base function `setMethod()` will later make generic. The
# interpreter runs a unit once, so a placeholder that another environment shares still finds
# the name bound after its unit ran.
.ry_s4_placeholder <- function(state, name, generic) {
  # The caller's argument reads its loop variable, which the promise would see at its last value.
  force(generic)
  delayedAssign(name, {
    .Internal(ryLoaderForceUnit(state$root, generic))
    bound <- exists(name, envir = state$ns, inherits = FALSE)
    get(name, envir = if (bound) state$ns else parent.env(state$ns))
  }, assign.env = state$ns)
}

.ry_s4_failed_placeholder <- function(state, name) {
  package <- methods::getPackageName(state$ns)
  delayedAssign(name, stop(sprintf(
    "`%s` is unavailable because loading the S4 definitions of %s failed; fix the error above and run `load_all()` again",
    name, package
  ), call. = FALSE), assign.env = state$ns)
}

# Runs held-back units, each once: the first unit when `generic` is `NULL` or not split off,
# and otherwise the first unit if it has not run and then that generic's. `all` runs every
# unit left, which a reload that reaches the definitions needs. During the load that deferred
# them, every definition placed before the statement now running runs here, and that
# statement's loop runs the rest in their places.
.ry_force_s4 <- function(state, generic = NULL, all = FALSE) {
  deferred <- state$s4_deferred
  if (is.null(deferred)) {
    return(invisible())
  }
  ns <- state$ns
  held <- .Internal(ryLoaderDeferred(state$root))
  previous <- options(topLevelEnvironment = ns)
  on.exit(options(previous), add = TRUE)
  run <- function(k) {
    withCallingHandlers(
      .Internal(ryLoaderRunDeferred(state$root, ns, k)),
      error = function(error) {
        message(sprintf("Error while loading %s:%d", held$path[[k]], held$line[[k]]))
      }
    )
  }
  if (!is.null(state$s4_cursor)) {
    state$s4_deferred <- NULL
    on.exit(.Internal(ryLoaderUndefer(state$root)), add = TRUE)
    .ry_restore_s4_labels(state)
    .ry_remove_s4_placeholders(state, deferred$names)
    for (k in seq_along(held$position)) {
      if (held$position[[k]] >= state$s4_cursor) {
        break
      }
      run(k)
    }
    return(invisible())
  }
  last <- proc.time()[["elapsed"]]
  step <- function(label) {
    if (.ry_verbose()) {
      now <- proc.time()[["elapsed"]]
      # Not a message, which would count as one the run signalled.
      cat(sprintf("  %-14s %7.1f ms\n", label, (now - last) * 1000), file = stderr())
      last <<- now
    }
  }
  lazy <- if (is.null(generic)) 0L else match(generic, deferred$lazy_generics, nomatch = 0L)
  units <- if (all) seq_along(state$s4_ran) - 1L else unique(c(0L, lazy))
  units <- units[!state$s4_ran[units + 1L]]
  unit_names <- function(unit) {
    if (unit == 0L) deferred$first_names else deferred$lazy_names[[unit]]
  }
  # The first unit's `cacheMetaData()` visits every methods table in the namespace, so the
  # placeholders of a lazy generic this force also runs go first: that generic's own
  # placeholder may be the promise under evaluation that started the force.
  .ry_remove_s4_placeholders(state, unlist(lapply(units[units != 0L], unit_names)))
  # A unit that fails leaves the lazy generics forced with it unbound, and nothing defers them
  # again, so their names report the failure instead of a missing function.
  finished <- integer()
  on.exit({
    for (unit in setdiff(units[units != 0L], finished)) {
      names <- unit_names(unit)
      for (name in names[!vapply(names, exists, NA, envir = ns, inherits = FALSE)]) {
        .ry_s4_failed_placeholder(state, name)
      }
      if (!is.null(state$package_env)) {
        .ry_sync_package_env(state, names, keeps_names = TRUE)
      }
    }
  }, add = TRUE)
  for (unit in units) {
    # A unit can reach another, which then runs inside it.
    if (state$s4_ran[[unit + 1L]]) {
      next
    }
    state$s4_ran[[unit + 1L]] <- TRUE
    names <- unit_names(unit)
    if (unit == 0L) {
      .ry_restore_s4_labels(state)
    }
    .ry_remove_s4_placeholders(state, names)
    if (unit == 0L) step("S4 lookup")
    for (k in which(deferred$units == unit)) {
      run(k)
    }
    if (unit == 0L) {
      step("S4 run")
      methods::cacheMetaData(ns, TRUE, ns)
      step("S4 caches")
      .ry_sync_exports(ns, state$namespace_spec, state$export_all)
    } else {
      name <- deferred$lazy_generics[[unit]]
      if (!.Internal(ryS4CacheGeneric(ns, name))) {
        .ry_cache_generic(ns, methods::getPackageName(ns), name, methods::getPackageName(ns))
      }
      # The first unit's export sync ran while this generic's names were unbound.
      if (0L %in% units) {
        .ry_sync_exports(ns, state$namespace_spec, state$export_all)
      }
    }
    if (!is.null(state$package_env)) {
      .ry_sync_package_env(state, names, keeps_names = TRUE)
    }
    if (unit == 0L) step("S4 exports")
    finished <- c(finished, unit)
  }
  if (all(state$s4_ran)) {
    state$s4_deferred <- NULL
    .Internal(ryLoaderUndefer(state$root))
  }
  invisible()
}

# The attached package environment shares the placeholder bindings, and the methods package
# searches it too, so a copy left there would make the definitions read their own placeholder.
.ry_remove_s4_placeholders <- function(state, names) {
  for (environment in list(state$ns, state$package_env)) {
    if (length(names) && !is.null(environment)) {
      rm(list = names[names %in% names(environment)], envir = environment)
    }
  }
}

# The body of `methods::cacheMetaData()`'s loop over generics, for attaching `ns`.
.ry_cache_generic <- function(ns, package, name, generic_package) {
  if (!identical(generic_package, package)) {
    if (exists(name, envir = ns, inherits = FALSE)) {
      definition <- get(name, envir = ns)
      generic <- methods:::.genericOrImplicit(name, generic_package, ns)
      if (is.function(definition)) {
        if (identical(environment(definition), environment(generic))) {
          return(invisible())
        } else if (methods::is(generic, "genericFunction")) {
          methods:::.assignOverBinding(name, generic, ns, FALSE)
        }
      }
    } else {
      generic <- methods::getGeneric(name, FALSE, ns, generic_package)
    }
  } else {
    generic <- methods::getGeneric(name, FALSE, ns, generic_package)
  }
  if (!methods::is(generic, "genericFunction")) {
    return(invisible())
  }
  methods:::.cacheGeneric(name, generic)
  tables <- methods:::.updateMethodsInTable(generic, ns, TRUE)
  methods::cacheGenericsMetaData(name, generic, TRUE, ns, generic@package, tables)
  invisible()
}

# A reload that reruns or undoes an S4 registration, or rebinds a name the held-back
# definitions bind, applies to them as already run.
.ry_reaches_deferred_s4 <- function(state, plan) {
  deferred <- state$s4_deferred
  !is.null(deferred) && (isTRUE(plan$registers_s4) || length(plan$unregister) > 0L ||
    any(c(plan$remove, plan$bind_name) %in% deferred$names))
}

# A fresh load replaces the namespace, so what the old one held back is never needed: running
# it would only register classes that the teardown removes again.
.ry_drop_deferred_s4 <- function(state) {
  deferred <- state$s4_deferred
  if (is.null(deferred)) {
    return(invisible())
  }
  state$s4_deferred <- NULL
  .ry_restore_s4_labels(state)
  .Internal(ryLoaderUndefer(state$root))
  rm(list = deferred$names[deferred$names %in% names(state$ns)], envir = state$ns)
}

# Builds the imports in a fresh environment, so a name an exporter dropped goes too, and a
# directive that fails leaves the namespace with the imports it had. Copying the old bindings
# aside instead would force the promises that lazy definitions import as.
.ry_import <- function(ns, name, imports) {
  previous <- parent.env(ns)
  previous_info <- getNamespaceInfo(ns, "imports")
  environment <- new.env(parent = parent.env(previous), hash = TRUE)
  attr(environment, "name") <- attr(previous, "name")
  parent.env(ns) <- environment
  done <- FALSE
  on.exit(if (!done) {
    parent.env(ns) <- previous
    setNamespaceInfo(ns, "imports", previous_info)
  })
  canonical <- list(base = TRUE)
  for (directive in imports) {
    package <- directive$package
    exporter <- loadNamespace(package)
    switch(directive$kind,
      namespace = {
        namespaceImport(ns, exporter, from = name, except = directive$except)
        canonical[[package]] <- TRUE
      },
      names = {
        namespaceImportFrom(ns, exporter, directive$names, from = name)
        if (!isTRUE(canonical[[package]])) {
          canonical[[package]] <- c(canonical[[package]], directive$names)
        }
      },
      classes = namespaceImportClasses(ns, exporter, directive$names, from = name),
      methods = namespaceImportMethods(ns, exporter, directive$names, from = name)
    )
  }
  setNamespaceInfo(ns, "imports", canonical)
  assign("system.file", system.file, envir = environment)
  done <- TRUE
}

.ry_unregister <- function(ns, registration) {
  if (!isNamespaceLoaded("methods") && !identical(registration$kind, "s3_method")) {
    return(invisible())
  }
  try(silent = TRUE, switch(registration$kind,
    s4_method = methods::removeMethod(registration$generic, registration$signature, where = ns),
    s4_class = methods::removeClass(registration$class, where = ns),
    s4_generic = methods::removeGeneric(registration$generic, where = ns),
    s3_method = .ry_unregister_s3(ns, list(generic = registration$generic,
                                           class = registration$class)),
    NULL
  ))
  invisible()
}

.ry_has_s4_metadata <- function(ns) {
  names <- names(ns)
  any(startsWith(names, ".__C__")) || any(startsWith(names, ".__T__")) ||
    any(startsWith(names, ".__A__"))
}

# The table entry is a promise for the namespace binding, so registering a method does not
# force its lazy definition. A reload re-registers every method it rebinds. A declared method
# whose function the package does not define refuses the load, as R does.
.ry_register_s3 <- function(ns, name, method) {
  if (!exists(method$`function`, envir = ns, inherits = FALSE)) {
    stop(sprintf("object '%s' not found whilst loading namespace '%s'", method$`function`, name),
         call. = FALSE)
  }
  generic <- method$generic
  .ry_remove_s3_hook(name, method)
  if (grepl("::", generic, fixed = TRUE)) {
    package <- sub("::.*$", "", generic)
    generic <- sub("^.*::", "", generic)
    if (!isNamespaceLoaded(package)) {
      hook <- function(...) {
        .ry_register_s3_in(ns, generic, method$class, method$`function`, asNamespace(package))
      }
      setHook(packageEvent(package, "onLoad"), hook)
      assign(.ry_s3_hook_key(name, method), list(package = package, hook = hook),
             envir = .ry_s3_hooks)
      return(invisible())
    }
    .ry_register_s3_in(ns, generic, method$class, method$`function`, asNamespace(package))
  } else {
    .ry_register_s3_in(ns, generic, method$class, method$`function`, ns)
  }
}

.ry_register_s3_in <- function(ns, generic, class, function_name, lookup) {
  table <- .ry_s3_table(generic, lookup)
  if (is.null(table)) {
    return(invisible())
  }
  force(function_name)
  delayedAssign(paste(generic, class, sep = "."),
                get(function_name, envir = ns, inherits = FALSE), assign.env = table)
}

# The delayed registrations `.ry_register_s3` left for packages not yet loaded, by package,
# generic and class, so a reload replaces its hook instead of adding another.
.ry_s3_hooks <- new.env(hash = TRUE, parent = emptyenv())

.ry_s3_hook_key <- function(name, method) {
  paste(name, method$generic, method$class, sep = "\r")
}

.ry_remove_s3_hook <- function(name, method) {
  key <- .ry_s3_hook_key(name, method)
  entry <- .ry_s3_hooks[[key]]
  if (is.null(entry)) {
    return(invisible())
  }
  rm(list = key, envir = .ry_s3_hooks)
  event <- packageEvent(entry$package, "onLoad")
  kept <- Filter(function(hook) !identical(hook, entry$hook), getHook(event))
  setHook(event, kept, "replace")
}

.ry_unregister_s3 <- function(ns, method) {
  .ry_remove_s3_hook(getNamespaceName(ns), method)
  generic <- sub("^.*::", "", method$generic)
  lookup <- ns
  if (grepl("::", method$generic, fixed = TRUE)) {
    package <- sub("::.*$", "", method$generic)
    if (!isNamespaceLoaded(package)) {
      return(invisible())
    }
    lookup <- asNamespace(package)
  }
  table <- tryCatch(.ry_s3_table(generic, lookup, quiet = TRUE), error = function(error) NULL)
  entry <- paste(generic, method$class, sep = ".")
  if (!is.null(table) && exists(entry, envir = table, inherits = FALSE)) {
    rm(list = entry, envir = table)
  }
}

# Where R's own registerS3methods() puts a method: the table of the environment that defines
# the generic, or base's for group generics and primitives.
.ry_s3_table <- function(generic, lookup, quiet = FALSE) {
  defining <- if (generic %in% c("Math", "Ops", "Summary", "Complex", "matrixOps")) {
    .BaseNamespaceEnv
  } else {
    function_object <- get0(generic, envir = lookup)
    if (is.null(function_object)) {
      if (quiet) return(NULL)
      warning(sprintf("S3 method for `%s` not registered: no such generic is visible", generic),
              call. = FALSE)
      return(NULL)
    }
    if (isS4(function_object) && methods::is(function_object, "genericFunction")) {
      function_object <- function_object@default
    }
    if (typeof(function_object) == "closure") environment(function_object) else .BaseNamespaceEnv
  }
  table <- defining[[".__S3MethodsTable__."]]
  if (is.null(table)) {
    table <- new.env(hash = TRUE, parent = baseenv())
    defining[[".__S3MethodsTable__."]] <- table
  }
  table
}

.ry_load_compiled <- function(state, libraries) {
  reload <- length(state$dlls) > 0L
  for (dll in state$dlls) {
    try(dyn.unload(dll[["path"]]), silent = TRUE)
  }
  state$dlls <- list()
  ns <- state$ns
  # The symbols the last load bound go with the libraries they point into. A name the package's
  # R code has bound since is the code's, and a check of it forces at most that one definition.
  for (name in names(state$native_symbols)) {
    if (exists(name, envir = ns, inherits = FALSE) &&
        identical(get(name, envir = ns, inherits = FALSE), state$native_symbols[[name]])) {
      rm(list = name, envir = ns)
    }
  }
  state$native_symbols <- list()
  if (!length(libraries)) {
    return(invisible())
  }
  .ry_compile(state$root)
  # As in `loadNamespace`, a symbol never replaces a name the namespace already defines.
  bind <- function(name, symbol) {
    if (exists(name, envir = ns, inherits = FALSE)) {
      warning(sprintf(
        "failed to assign RegisteredNativeSymbol for %s to %s since %s is already defined in the %s namespace",
        symbol$name, name, name, sQuote(state$name)
      ), call. = FALSE)
    } else {
      assign(name, symbol, envir = ns)
      state$native_symbols[[name]] <- symbol
    }
  }
  for (library in libraries) {
    path <- file.path(state$root, "src", paste0(library$library, .Platform$dynlib.ext))
    dll <- dyn.load(.ry_loadable(path, reload))
    state$dlls[[library$library]] <- dll
    if (isTRUE(library$registration)) {
      routines <- getDLLRegisteredRoutines(dll)
      for (kind in routines) {
        for (routine in names(kind)) {
          bind(paste0(library$fixes, routine), kind[[routine]])
        }
      }
    }
    for (i in seq_along(library$symbol_names)) {
      bind(paste0(library$fixes, library$symbol_names[[i]]),
           getNativeSymbolInfo(library$symbols[[i]], dll))
    }
  }
  setNamespaceInfo(ns, "DLLs", state$dlls)
}

.ry_export_names <- function(ns, spec, export_all) {
  all_names <- names(ns)
  if (isTRUE(export_all)) {
    return(all_names[!(all_names %in% .ry_stoplist)])
  }
  names <- spec$exports
  for (pattern in spec$export_patterns) {
    names <- c(names, grep(pattern, all_names, value = TRUE))
  }
  classes <- spec$export_classes
  for (pattern in spec$export_class_patterns) {
    classes <- c(classes, sub("^\\.__C__", "", grep(pattern, grep("^\\.__C__", all_names,
                                                                  value = TRUE), value = TRUE)))
  }
  if (length(classes)) names <- c(names, paste0(".__C__", classes))
  for (generic in spec$export_methods) {
    names <- c(names, intersect(generic, all_names),
               grep(paste0("^\\.__T__", generic, ":"), all_names, value = TRUE))
  }
  names <- unique(names[names %in% all_names])
  names[!(names %in% .ry_stoplist)]
}

.ry_sync_exports <- function(ns, spec, export_all) {
  exports <- getNamespaceInfo(ns, "exports")
  wanted <- .ry_export_names(ns, spec, export_all)
  have <- names(exports)
  stale <- have[!(have %in% wanted)]
  if (length(stale)) rm(list = stale, envir = exports)
  fresh <- wanted[!(wanted %in% have)]
  if (length(fresh)) list2env(stats::setNames(as.list(fresh), fresh), envir = exports)
}

.ry_attach <- function(plan, state) {
  name <- paste0("package:", state$name)
  environment <- attach(NULL, name = name, warn.conflicts = FALSE)
  attr(environment, "path") <- state$root
  state$package_env <- environment
  .ry_sync_package_env(state)
  if (exists(".onAttach", envir = state$ns, inherits = FALSE)) {
    get(".onAttach", envir = state$ns, inherits = FALSE)(dirname(state$root), state$name)
  }
}

# Copies bindings, not values, so a lazy definition stays a promise on the search path too:
# whichever environment forces it first, both see the value. Stale names always go, since
# running or undoing code (S4 registrations, `assign()`) changes names no plan lists, but the
# names the testthat helpers bound there stay until the helpers run again; `touched`
# limits the copying to the bindings a plan changed, and without it everything is copied.
# Datasets come last and win over an export of the same name, as in R's attachNamespace(). A
# plan that reloads them lists no `touched`, so only a full copy needs them.
.ry_sync_package_env <- function(state, touched = NULL, keeps_names = FALSE) {
  environment <- state$package_env
  ns <- state$ns
  exports <- getNamespaceInfo(ns, "exports")
  # With the same names as before, nothing is stale or missing, and only the rebound
  # bindings need copying.
  if (keeps_names) {
    names <- touched[vapply(touched, exists, NA, envir = exports, inherits = FALSE)]
    if (length(names)) .ry_internal("importIntoEnv", environment, names, ns, names)
    return(invisible())
  }
  exported <- names(exports)
  imported <- if (isTRUE(state$export_all)) names(parent.env(ns)) else character()
  lazydata <- getNamespaceInfo(ns, "lazydata")
  datasets <- names(lazydata)
  present <- names(environment)
  stale <- present[!(present %in% exported) & !(present %in% imported) &
                     !(present %in% datasets) & !(present %in% state$helper_names)]
  if (length(stale)) rm(list = stale, envir = environment)
  if (!is.null(touched)) {
    names <- c(touched[touched %in% exported], exported[!(exported %in% present)])
    if (length(names)) .ry_internal("importIntoEnv", environment, names, ns, names)
    return(invisible())
  }
  imported <- imported[!(imported %in% exported)]
  if (length(imported)) {
    .ry_internal("importIntoEnv", environment, imported, parent.env(ns), imported)
  }
  if (length(exported)) {
    .ry_internal("importIntoEnv", environment, exported, ns, exported)
  }
  if (length(datasets)) {
    .ry_internal("importIntoEnv", environment, datasets, lazydata, datasets)
  }
}

.ry_summary <- function(plan, elapsed, held = 0L, helpers = FALSE) {
  count <- function(n, singular, plural) sprintf("%d %s", n, if (n == 1L) singular else plural)
  timing <- sprintf("in %.0f ms", elapsed * 1000)
  if (plan$full) {
    parts <- c(
      sprintf("%s bound lazily", count(length(plan$bind_name), "definition", "definitions")),
      sprintf("%s run", count(length(plan$run_path) - held, "top-level statement",
                              "top-level statements")),
      if (held > 0L) {
        sprintf("%s held back until first use", count(held, "S4 definition", "S4 definitions"))
      }
    )
    return(sprintf("Loaded %s: %s, %s.", plan$package, paste(parts, collapse = ", "), timing))
  }
  parts <- c(
    if (length(plan$bind_name)) count(length(plan$bind_name), "definition rebound", "definitions rebound"),
    if (length(plan$run_path)) count(length(plan$run_path), "statement rerun", "statements rerun"),
    if (length(plan$remove)) count(length(plan$remove), "binding removed", "bindings removed"),
    if (length(plan$unregister)) {
      count(length(plan$unregister), "registration undone", "registrations undone")
    },
    if (!is.null(plan$imports)) "imports rebuilt",
    if (!is.null(plan$data)) "datasets reloaded",
    if (length(plan$s3_register) || length(plan$s3_unregister)) {
      count(length(plan$s3_register) + length(plan$s3_unregister), "S3 method re-registered",
            "S3 methods re-registered")
    }
  )
  if (!length(parts)) parts <- if (helpers) "testthat helpers sourced" else "exports refreshed"
  sprintf("Reloaded %s: %s, %s.", plan$package, paste(parts, collapse = ", "), timing)
}
