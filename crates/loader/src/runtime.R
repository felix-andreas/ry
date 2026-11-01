# ry's package loader, R side, shared by both runtimes. It applies plans that ry computes from
# its analysis of the package: every function definition, and every pure constant of a strict
# file, becomes a promise that evaluates only that definition on first use, and other top-level
# code runs in collation order. A reload
# applies a plan that touches only the items that changed.
#
# A backend file follows this one and defines how code reaches R: `gnur.R` parses source text in
# R, and `ry.R` hands ry's interpreter the analysis's syntax trees. The host defines the
# transport, `.ry_request(root, full)` (which returns the plan) and `.ry_commit(root)`, and
# creates the session's state, `.ry_packages` (one entry per loaded package root) and
# `.ry_timing`, since a host may cache the compiled functions across sessions and state must
# not be.

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
  plan <- .ry_request(root, full)
  .ry_phase("plan")
  if (!is.null(plan$error)) {
    stop(plan$error, call. = FALSE)
  }
  if (length(plan$warnings)) {
    for (warning_text in plan$warnings) warning(warning_text, call. = FALSE)
  }
  # Switching `export_all` changes what is exported even when no file changed.
  resync <- !identical(state$export_all, export_all)
  if (!plan$full && plan$noop && current && !resync) {
    .ry_commit(root)
    if (!quiet && report_noop) {
      message(sprintf("%s is up to date.", plan$package))
    }
    return(invisible(state$ns))
  }
  if (plan$full) {
    state <- .ry_new_package(plan, root)
  }
  state$export_all <- export_all
  # Exports are resynced by scanning every name in the namespace, which a reload that only
  # rebinds existing names cannot change.
  plan$keeps_names <- isTRUE(plan$keeps_names) && !resync
  # A statement that fails leaves the namespace partly updated, so the attached package
  # environment follows it either way rather than mixing two versions.
  applied <- FALSE
  on.exit(if (!applied && !is.null(state$package_env)) .ry_sync_package_env(state), add = TRUE)
  .ry_apply(plan, state)
  applied <- TRUE
  if (plan$full && isTRUE(attach)) {
    .ry_attach(plan, state)
  } else if (!is.null(state$package_env)) {
    .ry_sync_package_env(state, if (resync) NULL else plan$touched, plan$keeps_names)
  }
  .ry_phase("attach")
  .ry_commit(root)
  if (!quiet) {
    elapsed <- proc.time()[["elapsed"]] - started
    message(.ry_summary(plan, elapsed))
  }
  invisible(state$ns)
}

# With `options(ry.load.verbose = TRUE)`, reports how long each phase of a load took.
.ry_phase <- function(label) {
  now <- proc.time()[["elapsed"]]
  if (!is.null(label) && isTRUE(getOption("ry.load.verbose"))) {
    message(sprintf("  %-14s %7.1f ms", label, (now - .ry_timing$last) * 1000))
  }
  .ry_timing$last <- now
}

# Reloads every package loaded through ry whose sources changed. `ry repl` calls it before
# each command when it sees a change on disk, and it keeps `.Last.value`.
.ry_autoreload <- function() {
  last <- .Last.value
  for (root in ls(.ry_packages, all.names = TRUE)) {
    tryCatch(
      .ry_load(root, .ry_packages[[root]]$export_all, attach = TRUE, full = FALSE,
               quiet = FALSE, report_noop = FALSE),
      error = function(error) {
        message("ry: reloading ", root, " failed: ", conditionMessage(error))
      }
    )
  }
  invisible(last)
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

# A namespace that other namespaces import cannot be unloaded, which is the normal case when
# several development packages import each other, so it is unregistered instead and its
# importers keep their copies until they reload.
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
  if (length(plan$run_path)) {
    previous <- options(topLevelEnvironment = ns)
    on.exit(options(previous), add = TRUE)
    for (i in seq_along(plan$run_path)) {
      withCallingHandlers(
        .ry_run(state, plan, i, keep),
        error = function(error) {
          message(sprintf("Error while loading %s:%d", plan$run_path[[i]], plan$run_line[[i]]))
        }
      )
    }
  }
  .ry_phase("run")
  if (!identical(plan$sysdata, "keep")) {
    if (length(state$sysdata)) {
      rm(list = intersect(state$sysdata, ls(ns, all.names = TRUE)), envir = ns)
    }
    state$sysdata <- if (identical(plan$sysdata, "load")) {
      load(file.path(state$root, "R", "sysdata.rda"), envir = ns)
    } else {
      character()
    }
  }
  if (isNamespaceLoaded("methods") && (plan$full || isTRUE(plan$registers_s4)) &&
      .ry_has_s4_metadata(ns)) {
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
  if (!plan$keeps_names) {
    .ry_sync_exports(ns, plan$namespace, state$export_all)
  }
  .ry_phase("exports")
  if (isTRUE(plan$run_on_load) && exists(".onLoad", envir = ns, inherits = FALSE)) {
    get(".onLoad", envir = ns, inherits = FALSE)(dirname(state$root), state$name)
  }
}

.ry_import <- function(ns, name, imports) {
  environment <- parent.env(ns)
  rm(list = ls(environment, all.names = TRUE), envir = environment)
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
# force its lazy definition. A reload re-registers every method it rebinds.
.ry_register_s3 <- function(ns, name, method) {
  generic <- method$generic
  if (grepl("::", generic, fixed = TRUE)) {
    package <- sub("::.*$", "", generic)
    generic <- sub("^.*::", "", generic)
    if (!isNamespaceLoaded(package)) {
      setHook(packageEvent(package, "onLoad"), function(...) {
        .ry_register_s3_in(ns, generic, method$class, method$`function`, asNamespace(package))
      })
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

.ry_unregister_s3 <- function(ns, method) {
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
  for (dll in state$dlls) {
    try(dyn.unload(dll[["path"]]), silent = TRUE)
  }
  state$dlls <- list()
  if (!length(libraries)) {
    return(invisible())
  }
  if (!requireNamespace("pkgbuild", quietly = TRUE)) {
    stop("compiling src/ needs the pkgbuild package, which is not installed", call. = FALSE)
  }
  pkgbuild::compile_dll(state$root, quiet = TRUE)
  ns <- state$ns
  for (library in libraries) {
    path <- file.path(state$root, "src", paste0(library$library, .Platform$dynlib.ext))
    dll <- dyn.load(path)
    state$dlls[[library$library]] <- dll
    if (isTRUE(library$registration)) {
      routines <- getDLLRegisteredRoutines(dll)
      for (kind in routines) {
        for (routine in names(kind)) {
          assign(paste0(library$fixes, routine), kind[[routine]], envir = ns)
        }
      }
    }
    for (i in seq_along(library$symbol_names)) {
      assign(paste0(library$fixes, library$symbol_names[[i]]),
             getNativeSymbolInfo(library$symbols[[i]], dll), envir = ns)
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
# running or undoing code (S4 registrations, `assign()`) changes names no plan lists; `touched`
# limits the copying to the bindings a plan changed, and without it everything is copied.
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
  present <- names(environment)
  stale <- present[!(present %in% exported) & !(present %in% imported)]
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
}

.ry_summary <- function(plan, elapsed) {
  count <- function(n, singular, plural) sprintf("%d %s", n, if (n == 1L) singular else plural)
  timing <- sprintf("in %.0f ms", elapsed * 1000)
  if (plan$full) {
    return(sprintf("Loaded %s: %s bound lazily, %s run, %s.", plan$package,
                   count(length(plan$bind_name), "definition", "definitions"),
                   count(length(plan$run_path), "top-level statement", "top-level statements"),
                   timing))
  }
  parts <- c(
    if (length(plan$bind_name)) count(length(plan$bind_name), "definition rebound", "definitions rebound"),
    if (length(plan$run_path)) count(length(plan$run_path), "statement rerun", "statements rerun"),
    if (length(plan$remove)) count(length(plan$remove), "binding removed", "bindings removed"),
    if (length(plan$unregister)) {
      count(length(plan$unregister), "registration undone", "registrations undone")
    },
    if (!is.null(plan$imports)) "imports rebuilt",
    if (length(plan$s3_register) || length(plan$s3_unregister)) {
      count(length(plan$s3_register) + length(plan$s3_unregister), "S3 method re-registered",
            "S3 methods re-registered")
    }
  )
  if (!length(parts)) parts <- "exports refreshed"
  sprintf("Reloaded %s: %s, %s.", plan$package, paste(parts, collapse = ", "), timing)
}
