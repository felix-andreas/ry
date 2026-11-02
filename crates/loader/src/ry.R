# The ry runtime backend of the loader's R side. The plan stays in the ry process, and the
# interpreter lowers each definition and statement straight from the analysis's syntax tree,
# so no source text reaches R and nothing is parsed twice.

.ry_request <- function(root, full, conditions) .Internal(ryLoaderPlan(root, full, conditions))

.ry_commit <- function(root) invisible(.Internal(ryLoaderCommit(root)))

.ry_changed_roots <- function() .Internal(ryLoaderChanged())

.ry_await_verdict <- function() .Internal(ryLoaderAwaitVerdict())
.ry_callers <- function(root, names) .Internal(ryLoaderCallers(root, names))

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
# looks up one of their classes or generics, or a standard generic dispatches. A generic over an
# existing function announces itself as it is created, which must happen at load, so a load with
# one keeps them all eager. So does a method for another package's generic that dispatches
# without `standardGeneric()`: a primitive or a group generic, or a function that is not yet
# generic, which the method would turn into one.
.ry_may_defer_s4 <- function(state, deferred) {
  isTRUE(getOption("ry.load.lazy_s4", TRUE)) && isNamespaceLoaded("methods") &&
    !any(vapply(deferred$generics, exists, NA, envir = state$ns, mode = "function")) &&
    all(vapply(deferred$foreign_generics, function(name) {
      methods::is(get0(name, envir = state$ns, mode = "function"), "standardGeneric")
    }, NA))
}
