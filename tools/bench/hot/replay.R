# How a captured package entry is rebuilt, called and checked. bench.py prepends this file to
# harness.R and hot.py prepends it to validate.R, so the validator under GNU R and the harness on
# either engine replay a sample and fold its result into a checksum the same way.
#
# Everything lives in the one global `.hot`: a sample's environment has the global environment as
# its parent, so any other name defined here could shadow a name the sample looks up.
.hot <- local({
  # The value a sample that ends in an error gives in place of a result.
  failed <- structure(list(), class = "hot_error")

  # R names every session's temporary directory `Rtmp` and six letters or digits.
  session_directory <- "(^|/)Rtmp[[:alnum:]]{6}(/|$)"

  # A sample read for replay, with every path under the capture session's temporary directory
  # moved under this session's. A test writes its files under `tempdir()`, so a captured call
  # names directories that ended with the capture; replayed as captured, it would write into
  # the real /tmp outside any session, or fail because the directory is gone. The parent of
  # each moved path in the call or its variables is created, as the test had created it before
  # the call.
  relocate <- function(sample) {
    if (!length(grepRaw("Rtmp", serialize(list(sample$variables, sample$call, sample$value), NULL)))) {
      return(sample)
    }
    home <- tempdir()
    parents <- character()
    seen <- list()
    move <- function(text) {
      hit <- !is.na(text) & grepl(session_directory, text)
      if (any(hit)) {
        moved <- paste0(home, sub("^(.*/)?Rtmp[[:alnum:]]{6}", "", text[hit]))
        parents <<- c(parents, dirname(moved[moved != home]))
        text[hit] <- moved
      }
      text
    }
    walk <- function(x) {
      type <- typeof(x)
      if (type == "environment") {
        if (isNamespace(x) || identical(x, globalenv()) || identical(x, baseenv()) || identical(x, emptyenv()) ||
            any(vapply(seen, identical, NA, x))) {
          return(x)
        }
        seen[[length(seen) + 1L]] <<- x
        for (name in ls(x, all.names = TRUE, sorted = FALSE)) {
          if (bindingIsActive(name, x) || bindingIsLocked(name, x)) next
          value <- get(name, envir = x, inherits = FALSE)
          if (identical(value, quote(expr = ))) next
          walked <- walk(value)
          if (!identical(walked, value)) assign(name, walked, envir = x)
        }
        return(x)
      }
      if (type == "closure") {
        walk(environment(x))
        return(x)
      }
      if (type %in% c("builtin", "special", "externalptr", "weakref", "bytecode", "symbol", "NULL")) return(x)
      if (type == "language") {
        for (k in seq_along(x)) {
          if (!identical(x[[k]], quote(expr = ))) {
            walked <- walk(x[[k]])
            if (!identical(walked, x[[k]])) x[[k]] <- walked
          }
        }
        return(x)
      }
      found <- attributes(x)
      walked_attributes <- if (length(found)) lapply(found, walk) else found
      if (type == "character") {
        moved <- move(x)
        if (!identical(moved, x)) x[] <- moved
      } else if (type == "list") {
        bare <- x
        attributes(bare) <- NULL
        walked <- lapply(bare, walk)
        if (!identical(walked, bare)) {
          x <- walked
          attributes(x) <- found
        }
      }
      if (!identical(walked_attributes, found)) attributes(x) <- walked_attributes
      x
    }
    sample$variables <- walk(sample$variables)
    sample$call <- walk(sample$call)
    # Only the arguments' paths get their parents: the validator reads the captured value, which
    # the suite's samples do not keep, so a directory made for a path in it would let a call pass
    # the validator that fails in the harness.
    created <- parents
    sample$value <- walk(sample$value)
    for (parent in unique(created)) dir.create(parent, recursive = TRUE, showWarnings = FALSE)
    sample
  }

  # The sample's call evaluates in a fresh environment that holds its free variables and the
  # names it imported from packages, the way the recorder describes a replay.
  environment_of <- function(sample, variables = sample$variables) {
    env <- if (length(variables)) list2env(variables, parent = globalenv()) else new.env(parent = globalenv())
    imports <- sample$imports
    for (name in names(imports)) {
      assign(name, get(name, envir = asNamespace(imports[[name]])), envir = env)
    }
    env
  }

  # A relocated sample made ready for batches: its environment, built once, and for a `fresh`
  # sample (one whose call changes its variables in place, such as data.table's `:=` or an R6
  # object's fields) its variables serialized, so every call can get a copy of its own.
  ready <- function(sample, iterations, rng, threw, fresh) {
    list(call = sample$call, env = environment_of(sample), imports = sample$imports,
         bytes = if (fresh) serialize(sample$variables, NULL), seed = if (rng) sample$seed,
         threw = threw, iterations = iterations, varies = isTRUE(sample$varies))
  }

  # The environments of one batch, one per call. A sample that is not fresh calls in the same
  # environment every time. A fresh one gets a copy of its variables for every call, which costs
  # an unserialize per call and is why this step runs before the batch and outside its timing.
  prepare <- function(samples) {
    lapply(samples, function(sample) {
      envs <- if (is.null(sample$bytes)) {
        rep(list(sample$env), sample$iterations)
      } else {
        lapply(seq_len(sample$iterations), function(i) environment_of(sample, unserialize(sample$bytes)))
      }
      list(call = sample$call, envs = envs, seed = sample$seed, threw = sample$threw)
    })
  }

  # One batch: every prepared sample called once in each of its environments, restoring the seed
  # before each call of a sample that draws random numbers. The loops are written out per case
  # so the timed loop does no test it does not need.
  batch <- function(prepared) {
    last <- vector("list", length(prepared))
    for (k in seq_along(prepared)) {
      sample <- prepared[[k]]
      call <- sample$call
      seed <- sample$seed
      value <- NULL
      if (sample$threw) {
        if (is.null(seed)) {
          for (env in sample$envs) value <- tryCatch(eval(call, env), error = function(condition) failed)
        } else {
          for (env in sample$envs) {
            assign(".Random.seed", seed, envir = globalenv())
            value <- tryCatch(eval(call, env), error = function(condition) failed)
          }
        }
      } else if (is.null(seed)) {
        for (env in sample$envs) value <- eval(call, env)
      } else {
        for (env in sample$envs) {
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

  # A prepared batch under those handlers, established once for all its calls.
  run <- function(prepared) handled(batch(prepared))

  # A checksum every engine computes alike: `<kind>:<length>:<hash>:<magnitude>:<balance>`, or
  # `null`, `error`, `opaque:<type>`, `lang:<hash>` or `unhashable`. The hash covers everything
  # but the numbers: the types, lengths, attributes, strings, logical values, and how many NA,
  # NaN, Inf and -Inf each numeric vector holds. Environments, functions and external pointers
  # hold addresses, so they fold to their type alone, and so does an attribute that holds one.
  #
  # Every numeric vector, at any depth, adds to the last two fields, which bench.py compares to
  # an absolute 1e-10 (and a relative 1e-13, for a sum of many vectors): the logarithm of its
  # weighted size, and a sum under a second set of weights over that size. Both are exact
  # functions of the vector, so a difference that moves either sum by a relative 1e-10 shows,
  # while the last-bit differences of two engines' math libraries do not (`power.t.test` finds
  # its root through `uniroot`, and the two engines' roots differ by one part in 1e13). One wrong
  # element of a long vector therefore shows only when it is that large a part of the whole.
  # Each vector's fields are weighted by its position among the vectors, in the same way, so
  # moving a number from one vector to another shows too.
  checksum <- function(value) {
    if (is.null(value)) return("null")
    if (inherits(value, "hot_error")) return("error")
    type <- typeof(value)
    if (type %in% addressed) return(paste0("opaque:", type))
    tryCatch({
      numbers <- new.env()
      numbers$count <- 0
      numbers$magnitude <- 0
      numbers$balance <- 0
      numbers$home <- tempdir()
      folded <- fold(value, numbers)
      if (type %in% c("symbol", "language")) return(folded)
      sprintf("%s:%d:%s:%.15e:%.15e", kinds[[if (isS4(value) && !is.list(value)) "S4" else type]],
              length(value), hash(folded), numbers$magnitude, numbers$balance)
    }, error = function(condition) "unhashable")
  }

  addressed <- c("environment", "closure", "builtin", "special", "externalptr", "weakref", "bytecode")
  kinds <- c(logical = "lgl", integer = "int", double = "dbl", complex = "cpl", character = "chr", raw = "raw",
             list = "list", pairlist = "list", expression = "list", S4 = "s4")
  # Attributes that hold an address of the engine's own: data.table's pointer to itself.
  unstable_attributes <- ".internal.selfref"

  # A text that describes `value` but its numbers, which go to `numbers`.
  fold <- function(value, numbers) {
    if (is.null(value)) return("null")
    type <- typeof(value)
    if (type %in% addressed) return(paste0("opaque:", type))
    if (type %in% c("symbol", "language")) return(paste0("lang:", hash(deparse(value))))
    if (isS4(value) && !is.list(value)) return(paste0("s4:", fold_attributes(value, numbers)))
    if (type == "pairlist") value <- as.list(value)
    described <- if (is.list(value)) {
      bare <- value
      attributes(bare) <- NULL
      parts <- vapply(bare, fold, "", numbers, USE.NAMES = FALSE)
      sprintf("list:%d:%s", length(bare), hash(parts))
    } else {
      switch(type,
        logical = sprintf("lgl:%d:%d:%s", length(value), sum(is.na(value)), hash(as.character(which(value)))),
        integer = ,
        double = fold_number(type, as.double(value), numbers),
        complex = fold_number(type, c(Re(value), Im(value)), numbers),
        character = paste0("chr:", fold_text(value, numbers$home)),
        raw = sprintf("raw:%d:%s", length(value), hash(as.character(as.integer(value)))),
        paste0("opaque:", type)
      )
    }
    paste0(described, "|", fold_attributes(value, numbers))
  }

  # The attributes in the order of their names, which no engine's storage order decides.
  fold_attributes <- function(value, numbers) {
    found <- attributes(value)
    found <- found[setdiff(names(found), unstable_attributes)]
    if (!length(found)) return("")
    found <- found[order(names(found), method = "radix")]
    hash(c(names(found), vapply(found, fold, "", numbers, USE.NAMES = FALSE)))
  }

  # The golden ratio's fractional part: the weight 1 + frac(k * phi) of the k-th position lies in
  # [1, 2), never repeats, and differs by a good part of 1 between near positions, so swapping
  # two numbers changes the sum unless they are equal.
  phi <- 0.6180339887498949
  # The balance has weights of its own, from the square root of two: with the size's weights it
  # would be exactly 1 for every vector without a negative number, and say nothing about it.
  root <- 0.4142135623730951

  fold_number <- function(type, values, numbers) {
    finite <- is.finite(values)
    weights <- 1 + (seq_along(values) * phi) %% 1
    size <- sum(abs(values[finite]) * weights[finite])
    if (size > 0) {
      numbers$count <- numbers$count + 1
      position <- 1 + (numbers$count * phi) %% 1
      numbers$magnitude <- numbers$magnitude + position * log(size)
      balance <- 1 + (seq_along(values) * root) %% 1
      numbers$balance <- numbers$balance + position * sum(values[finite] * balance[finite]) / size
    }
    nan <- is.nan(values)
    sprintf("%s:%d:%d:%d:%d:%d:%d", type, length(values), sum(is.na(values) & !nan), sum(nan),
            sum(values == Inf, na.rm = TRUE), sum(values == -Inf, na.rm = TRUE), as.integer(size > 0))
  }

  # Strings with this session's temporary directory in a fixed form, since a result that names
  # a path under it names another directory in every process.
  fold_text <- function(text, home) {
    missing <- is.na(text)
    text <- gsub(home, "<tempdir>", enc2utf8(text), fixed = TRUE)
    sprintf("%d:%d:%s", length(text), sum(missing), hash(ifelse(missing, "\001NA", text)))
  }

  # The bytes of the lines, each weighted by (7919 * position) modulo the prime 1048573, plus
  # one, summed exactly: the weight repeats only every 1048573 bytes. The sum is taken per
  # mebibyte and each part reduced modulo 2^31 - 1, so it stays below 2^53 for any length.
  hash <- function(lines) {
    bytes <- as.numeric(charToRaw(paste(lines, collapse = "\n")))
    positions <- seq_along(bytes)
    weighted <- bytes * ((positions * 7919) %% 1048573 + 1)
    parts <- vapply(split(weighted, (positions - 1) %/% 1048576), sum, 0)
    sprintf("%dx%.0f", length(bytes), sum(parts %% 2147483647))
  }

  environment()
})
