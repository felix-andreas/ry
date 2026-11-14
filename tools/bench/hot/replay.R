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

  # A batch with messages and warnings muffled by handlers established once, so their cost is a
  # signal, not a printed line. A warning signalled without a muffle restart passes on.
  run <- function(prepared) {
    withCallingHandlers(
      batch(prepared),
      message = function(condition) {
        restart <- findRestart("muffleMessage")
        if (!is.null(restart)) invokeRestart(restart)
      },
      warning = function(condition) {
        restart <- findRestart("muffleWarning")
        if (!is.null(restart)) invokeRestart(restart)
      }
    )
  }

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

  fold_number <- function(kind, numbers) {
    finite <- is.finite(numbers)
    weights <- seq_along(numbers) %% 7 + 1
    sprintf("%s:%d:%d:%.15g", kind, length(numbers), sum(!finite), sum(numbers[finite] * weights[finite]))
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
