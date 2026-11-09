# The ry runtime backend of the loader's R side. The plan stays in the ry process, and the
# interpreter lowers each definition and statement straight from the analysis's syntax tree,
# so no source text reaches R and nothing is parsed twice.

.ry_request <- function(root, full, conditions) .Internal(ryLoaderPlan(root, full, conditions))

.ry_commit <- function(root) invisible(.Internal(ryLoaderCommit(root)))

.ry_changed_roots <- function() .Internal(ryLoaderChanged())

.ry_await_verdict <- function() .Internal(ryLoaderAwaitVerdict())
.ry_callers <- function(root, names) .Internal(ryLoaderCallers(root, names))
.ry_wait_edits <- function(root, timeout) .Internal(ryLoaderWaitEdits(root, timeout))

.ry_sources <- function(state, plan) invisible()

# Compiled code builds with ry's toolchain against ry's R home, so it links ry's libR.
.ry_compile <- function(root) .Internal(ryLoaderCompile(root, .libPaths()))

# ry's `dyn.unload` keeps the old library mapped, and opening the same path again would return
# it, so a reload loads a copy of the rebuilt library from a directory of its own. The copy keeps
# the file name, which names the library's `R_init_` function.
.ry_loadable <- function(path, reload) {
  if (!reload) return(path)
  directory <- tempfile("ry-dll-")
  dir.create(directory)
  copy <- file.path(directory, basename(path))
  if (!file.copy(path, copy)) stop("cannot copy ", path, " to ", copy, call. = FALSE)
  copy
}

.ry_bind_lazily <- function(state, plan, keep) .Internal(ryLoaderBind(state$root, state$ns))

.ry_run <- function(state, plan, i, keep) .Internal(ryLoaderRun(state$root, state$ns, i))

# Holding back S4 definitions relies on the interpreter running them when the methods package
# looks up one of their classes or generics, or a primitive dispatches on an object of one of
# their classes. A generic over an existing function announces itself as it is created, which
# must happen at load, so a load with one keeps them all eager. So does a method for a group
# generic such as `Arith`, which its members dispatch to without any lookup the interpreter
# sees, or a method whose signature is longer than its generic's method tables, which adding
# it would rebuild.
.ry_s4_eager_reason <- function(state, deferred) {
  if (!isTRUE(getOption("ry.load.lazy_s4", TRUE))) {
    return("`options(ry.load.lazy_s4 = FALSE)` turns deferral off")
  }
  if (!isNamespaceLoaded("methods")) {
    return("the methods package is not loaded")
  }
  for (generic in deferred$generics) {
    if (exists(generic, envir = state$ns, mode = "function")) {
      return(sprintf("the generic `%s` already exists as a function the package sees", generic))
    }
  }
  for (method in deferred$foreign_methods) {
    generic <- get0(method$generic, envir = state$ns, mode = "function")
    kind <- .ry_foreign_generic_kind(generic)
    if (is.na(kind)) {
      return(sprintf("a method for `%s`, which is a group generic or not a function", method$generic))
    }
    if (kind == "standard" && is.na(.ry_s4_label(generic, method$signature))) {
      return(sprintf("a method for `%s` whose signature the loader cannot label", method$generic))
    }
    # A method for one of the package's own plain functions makes a generic of a binding the
    # load itself makes, which a placeholder would replace.
    if (kind == "implicit" && identical(topenv(environment(generic)), state$ns)) {
      return(sprintf("a method for the package's own function `%s`", method$generic))
    }
  }
  NULL
}

# How a method for another package's function `definition` is reached: through a generic that
# calls `standardGeneric()`, as `show` and `initialize` do, through a primitive's own dispatch, or
# through the generic that `setMethod()` makes of a plain function. A group generic such as
# `Arith` is `NA`.
.ry_foreign_generic_kind <- function(definition) {
  if (is.primitive(definition)) {
    "primitive"
  } else if (methods::is(definition, "groupGenericFunction")) {
    NA_character_
  } else if (methods::is(definition, "genericFunction")) {
    "standard"
  } else if (is.function(definition)) {
    "implicit"
  } else {
    NA_character_
  }
}

# The label under which the method tables of `generic` hold a method for `signature` (a list
# of classes named by argument where the source names them), or `NA` when adding it would
# lengthen every label.
.ry_s4_label <- function(generic, signature) {
  classes <- tryCatch(
    methods::matchSignature(unlist(signature), generic),
    error = function(condition) NULL
  )
  length <- get0(".SigLength", envir = environment(generic), inherits = FALSE, ifnotfound = 1L)
  if (is.null(classes) || length(classes) > length) {
    return(NA_character_)
  }
  paste(c(classes, rep("ANY", length - length(classes))), collapse = "#")
}
