# How a captured package entry is rebuilt, called and checked. bench.py prepends this file to
# harness.R and hot.py prepends it to validate.R, so the validator under GNU R and the harness on
# either engine replay a sample and fold its result into a checksum the same way.
#
# Everything lives in the one global `.hot`: a sample's environment has the global environment as
# its parent, so any other name defined here could shadow a name the sample looks up.
.hot <- local({
  # The value a sample that ends in an error gives in place of a result.
  failed <- structure(list(), class = "hot_error")

  # The sample's call evaluates in a fresh environment that holds its free variables and the
  # names it imported from packages, the way the recorder describes a replay.
  environment_of <- function(sample) {
    variables <- sample$variables
    env <- if (length(variables)) list2env(variables, parent = globalenv()) else new.env(parent = globalenv())
    imports <- sample$imports
    for (name in names(imports)) {
      assign(name, get(name, envir = asNamespace(imports[[name]])), envir = env)
    }
    env
  }

  # One batch: every prepared sample (`call`, `env`, `seed`, `threw`, `iterations`) called its
  # number of times, restoring the seed before each call of a sample that draws random numbers.
  # The loops are written out per case so the timed loop does no test it does not need.
  batch <- function(prepared) {
    last <- vector("list", length(prepared))
    for (k in seq_along(prepared)) {
      sample <- prepared[[k]]
      call <- sample$call
      env <- sample$env
      seed <- sample$seed
      count <- sample$iterations
      value <- NULL
      if (sample$threw) {
        for (i in seq_len(count)) {
          if (!is.null(seed)) assign(".Random.seed", seed, envir = globalenv())
          value <- tryCatch(eval(call, env), error = function(condition) failed)
        }
      } else if (is.null(seed)) {
        for (i in seq_len(count)) value <- eval(call, env)
      } else {
        for (i in seq_len(count)) {
          assign(".Random.seed", seed, envir = globalenv())
          value <- eval(call, env)
        }
      }
      last[k] <- list(value)
    }
    last
  }

  # Puts the process in the session the samples were captured in, as far as a call can tell,
  # and returns the packages of `namespaces` that did not load. A code base's tests run with the
  # packages it imports loaded, and loading a package changes what other calls do: it registers
  # S3 methods (`zoo::rollapply` of an xts series pads with NA once xts is loaded) and solver
  # plugins (ompr finds a solver only after ROI.plugin.highs is). So a replay loads all of them
  # before the first call, and a result does not depend on which samples ran earlier. A package
  # that is not installed is one the code bases ran without too.
  #
  # The code bases' runner (crates/oracle/src/bench.R) also sets testthat's third edition once.
  # An expectation that finds no edition searches for a package DESCRIPTION on every call, which
  # costs a hundred times the expectation itself. The edition is bound to an environment that is
  # never released, so it lasts for the session.
  session <- new.env()
  enter_session <- function(namespaces) {
    loads <- vapply(namespaces, function(namespace) {
      tryCatch({
        suppressPackageStartupMessages(loadNamespace(namespace))
        TRUE
      }, error = function(error) FALSE)
    }, NA)
    if (isNamespaceLoaded("testthat")) testthat::local_edition(3L, .env = session)
    namespaces[!loads]
  }

  # The handlers the runner establishes around a test: an expectation continues the test through
  # testthat's restart, and messages and warnings are muffled, so their cost is a signal and not
  # a printed line. A condition signalled without its restart passes on.
  handled <- function(expression) {
    resume <- function(name) {
      function(condition) {
        restart <- findRestart(name)
        if (!is.null(restart)) invokeRestart(restart)
      }
    }
    withCallingHandlers(
      expression,
      expectation = resume("continue_test"),
      message = resume("muffleMessage"),
      warning = resume("muffleWarning")
    )
  }

  # A batch under those handlers, established once for all its calls.
  run <- function(prepared) handled(batch(prepared))

  # A checksum every engine computes alike: `<kind>:<fields>`. A number sums in an order the
  # engines may not share, so a `num` or `cpl` checksum ends in a floating sum that bench.py
  # compares to a relative 1e-10; every other field must match exactly. Environments, functions
  # and external pointers hold addresses, so they fold to their type alone.
  checksum <- function(value) {
    tryCatch(fold(value), error = function(condition) "unhashable")
  }

  fold <- function(value) {
    if (is.null(value)) return("null")
    if (inherits(value, "hot_error")) return("error")
    if (is.factor(value)) return(paste0("fct:", fold_text(as.character(value))))
    type <- typeof(value)
    if (type %in% c("environment", "closure", "builtin", "special", "externalptr", "weakref", "bytecode")) {
      return(paste0("opaque:", type))
    }
    if (type %in% c("symbol", "language")) {
      return(paste0("lang:", hash(deparse(value))))
    }
    if (isS4(value) && !is.list(value)) {
      return(paste0("s4:", fold(attributes(value))))
    }
    if (type == "pairlist") value <- as.list(value)
    if (is.list(value)) {
      parts <- vapply(unclass(value), fold, "", USE.NAMES = FALSE)
      return(sprintf("list:%d:%s", length(value), hash(c(names(value), parts))))
    }
    switch(type,
      logical = sprintf("lgl:%d:%d:%s", length(value), sum(is.na(value)), hash(as.character(which(value)))),
      integer = ,
      double = fold_number("num", as.double(unclass(value))),
      complex = fold_number("cpl", c(Re(value), Im(value))),
      character = paste0("chr:", fold_text(value)),
      raw = sprintf("raw:%d:%s", length(value), hash(as.character(as.integer(value)))),
      paste0("opaque:", type)
    )
  }

  # Numbers are compared to twelve digits of their size, not to the last bit: the engines' math
  # libraries round differently in the last place, and an iterative solver carries that further
  # (`power.t.test` finds its root through `uniroot`, and the two roots differ by one part in 1e13).
  fold_number <- function(kind, numbers) {
    finite <- is.finite(numbers)
    weights <- seq_along(numbers) %% 7 + 1
    size <- sum(abs(numbers[finite]) * weights[finite])
    total <- sum(numbers[finite] * weights[finite])
    sprintf("%s:%d:%d:%.12g:%.11f", kind, length(numbers), sum(!finite), size, if (size > 0) total / size else 0)
  }

  fold_text <- function(text) {
    sprintf("%d:%d:%s", length(text), sum(is.na(text)), hash(ifelse(is.na(text), "\001NA", enc2utf8(text))))
  }

  # The bytes of the lines, each weighted by its position modulo a prime, summed exactly: for
  # up to a few megabytes the sum stays below 2^53.
  hash <- function(lines) {
    bytes <- as.numeric(charToRaw(paste(lines, collapse = "\n")))
    sprintf("%d:%.0f", length(bytes), sum(bytes * (seq_along(bytes) %% 251 + 1)))
  }

  environment()
})
