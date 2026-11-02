# The GNU R backend of the loader's R side: a plan carries each new source file's text, and
# code is parsed from it in R, at its real position in the file, so source references, and
# with them tracebacks and debuggers, point into the file on disk.

# A console on GNU R names the roots that changed in its call to `.ry_autoreload()`.
.ry_changed_roots <- function() ls(.ry_packages, all.names = TRUE)

# ry checks a package before GNU R loads it, so the load never waits for a verdict.
.ry_await_verdict <- function() invisible()

.ry_sources <- function(state, plan) {
  if (is.null(state$texts)) {
    state$texts <- new.env(parent = emptyenv())
    state$srcfiles <- new.env(parent = emptyenv())
  }
  for (path in plan$dropped_sources) {
    for (table in list(state$texts, state$srcfiles)) {
      if (exists(path, envir = table, inherits = FALSE)) rm(list = path, envir = table)
    }
  }
  for (i in seq_along(plan$source_path)) {
    path <- plan$source_path[[i]]
    assign(path, plan$source_text[[i]], envir = state$texts)
    if (exists(path, envir = state$srcfiles, inherits = FALSE)) rm(list = path, envir = state$srcfiles)
  }
}

.ry_parse <- function(state, path, line, column, text, keep) {
  if (keep) {
    previous <- options(keep.parse.data = FALSE)
    on.exit(options(previous))
    # Padding by the bytes of the line before the code keeps the byte offsets that printing a
    # function reads exact. A multibyte character there makes the character columns on that
    # line too large by its extra bytes, which `.ry_shift_columns` takes back.
    srcfile <- .ry_srcfile(state, path)
    prefix <- substr(srcfile$lines[[line]], 1L, column)
    bytes <- nchar(prefix, type = "bytes")
    padded <- c(character(line - 1L), paste0(strrep(" ", bytes), text))
    expressions <- parse(text = padded, srcfile = srcfile, keep.source = TRUE, encoding = "UTF-8")
    shift <- bytes - nchar(prefix, type = "chars")
    if (shift > 0L) .ry_shift_columns(expressions, line, shift) else expressions
  } else {
    parse(text = text, keep.source = FALSE, encoding = "UTF-8")
  }
}

# Moves every character column on `line` of the source references in `expressions` left by
# `shift`: those of each expression, of each `{` block's statements, and of each function.
.ry_shift_columns <- function(expressions, line, shift) {
  move <- function(reference) {
    if (!inherits(reference, "srcref")) return(reference)
    if (reference[[1L]] == line) reference[[5L]] <- reference[[5L]] - shift
    if (reference[[3L]] == line) reference[[6L]] <- reference[[6L]] - shift
    reference
  }
  # A function's formals hold the empty symbol for a parameter without a default, and a call
  # holds it for an empty argument, so only calls, formals and references are visited.
  move_all <- function(references) {
    if (is.list(references)) lapply(references, move) else move(references)
  }
  walk <- function(node) {
    references <- attr(node, "srcref")
    if (!is.null(references)) attr(node, "srcref") <- move_all(references)
    for (i in seq_along(node)) {
      if (is.call(node[[i]]) || is.pairlist(node[[i]]) && !is.null(node[[i]])) {
        node[[i]] <- walk(node[[i]])
      } else if (inherits(node[[i]], "srcref")) {
        node[[i]] <- move(node[[i]])
      }
    }
    node
  }
  for (i in seq_along(expressions)) {
    if (is.call(expressions[[i]])) expressions[[i]] <- walk(expressions[[i]])
  }
  attr(expressions, "srcref") <- move_all(attr(expressions, "srcref"))
  attr(expressions, "wholeSrcref") <- move(attr(expressions, "wholeSrcref"))
  expressions
}

# Built on first use: only a definition that is forced needs its file's lines.
.ry_srcfile <- function(state, path) {
  srcfile <- state$srcfiles[[path]]
  if (is.null(srcfile)) {
    lines <- strsplit(state$texts[[path]], "\n", fixed = TRUE)[[1L]]
    srcfile <- srcfilecopy(file.path(state$root, path), lines, isFile = TRUE)
    assign(path, srcfile, envir = state$srcfiles)
  }
  srcfile
}

# One promise per definition, made in C by the same internal that installed packages use for
# their lazy-load databases. A promise evaluates `.ry_force(i, batch)`, and `batch` keeps the
# file text each definition was cut from, so a later edit of the file cannot shift it.
.ry_bind_lazily <- function(state, plan, keep) {
  batch <- new.env(parent = environment(.ry_bind_lazily))
  batch$batch <- batch
  batch$state <- state
  batch$keep <- keep
  batch$path <- plan$bind_path
  batch$text <- unlist(mget(plan$bind_path, envir = state$texts), use.names = FALSE)
  batch$line <- plan$bind_line
  batch$column <- plan$bind_column
  batch$start <- plan$bind_start
  batch$end <- plan$bind_end
  keys <- as.list(seq_along(plan$bind_name))
  .ry_internal("makeLazy", plan$bind_name, keys, quote(.ry_force(key, batch)), batch, state$ns)
}

.ry_force <- function(i, batch) {
  text <- substr(batch$text[[i]], batch$start[[i]] + 1L, batch$end[[i]])
  expressions <- .ry_parse(batch$state, batch$path[[i]], batch$line[[i]], batch$column[[i]],
                           text, batch$keep)
  eval(expressions[[1L]], batch$state$ns)
}

.ry_run <- function(state, plan, i, keep) {
  path <- plan$run_path[[i]]
  text <- substr(state$texts[[path]], plan$run_start[[i]] + 1L, plan$run_end[[i]])
  expressions <- .ry_parse(state, path, plan$run_line[[i]], plan$run_column[[i]], text, keep)
  for (expression in expressions) eval(expression, state$ns)
}

# GNU R's methods package looks classes and generics up in C that offers no hook to run held
# back definitions from, so they always run at load.
.ry_may_defer_s4 <- function(state, deferred) FALSE

# GNU R builds compiled code with its own toolchain, through pkgbuild as `devtools::load_all`
# does.
.ry_compile <- function(root) {
  if (!requireNamespace("pkgbuild", quietly = TRUE)) {
    stop("compiling src/ needs the pkgbuild package, which is not installed", call. = FALSE)
  }
  pkgbuild::compile_dll(root, quiet = TRUE)
}

# `dyn.unload` closed the library a reload rebuilt, so it loads again from where it was built.
.ry_loadable <- function(path, reload) path
