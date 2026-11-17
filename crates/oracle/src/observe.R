# The deep observer: a canonical text dump of one R value, for comparing two engines on what
# printing and deparsing hide. `.oracle_observe(value)` returns one string with a line per node:
#
#   - the type, the length, and the object and S4 bits;
#   - every element: doubles as `NA`, `NaN` or their bits (so `-0` differs from `0`), strings as
#     their encoding mark (`U` UTF-8, `L` latin1, `B` bytes, `-` none) and their bytes. A
#     non-ASCII string that is valid UTF-8 shows `u` whether it is marked UTF-8 or unmarked,
#     because the two mean the same text in the UTF-8 session the harness runs, and which of
#     them a builtin returns is `Encoding()` trivia; `ORACLE_OBSERVE=strict` tells them apart;
#   - the attributes in the order `attributes()` gives them, each dumped the same way, and a data
#     frame's row names in their stored form;
#   - calls, pairlists and closures element by element, with a closure's formals, body and
#     kind of environment;
#   - an environment's kind and its sorted names, without reading a binding, because reading
#     forces promises and fires active bindings.
#
# Vectors longer than `.oracle_observe_limit` show their ends and, for the middle, checksums that
# depend on the position of every element.
# The dump uses only base functions, so it reads the same under any engine that runs base R.
.oracle_observe_limit <- 96L
.oracle_observe_strict <- identical(Sys.getenv("ORACLE_OBSERVE"), "strict")

.oracle_observe <- function(value) {
  lines <- character()
  emit <- function(depth, ...) lines[[length(lines) + 1L]] <<- paste0(strrep("  ", depth), ...)

  hex <- function(bytes) paste(as.character(bytes), collapse = "")

  string_token <- function(text) {
    if (is.na(text)) return("NA")
    mark <- switch(Encoding(text), "UTF-8" = "U", latin1 = "L", bytes = "B", "-")
    bytes <- charToRaw(text)
    plain <- all(bytes >= as.raw(0x20) & bytes < as.raw(0x7f)) && !any(bytes == as.raw(0x22) | bytes == as.raw(0x5c))
    if (plain) return(paste0(mark, "\"", rawToChar(bytes), "\""))
    if (!.oracle_observe_strict && mark %in% c("U", "-") && validUTF8(text)) mark <- "u"
    paste0(mark, "<", hex(bytes), ">")
  }

  double_tokens <- function(x) {
    tokens <- character(length(x))
    missing <- is.na(x)
    nan <- is.nan(x)
    tokens[missing & !nan] <- "NA"
    tokens[nan] <- "NaN"
    rest <- !missing
    if (any(rest)) {
      bits <- matrix(as.character(writeBin(as.double(x[rest]), raw(), endian = "big")), nrow = 8L)
      bits <- vapply(seq_len(ncol(bits)), function(column) paste(bits[, column], collapse = ""), "")
      tokens[rest] <- paste0(sprintf("%.17g", x[rest]), ":", bits)
    }
    tokens
  }

  tokens_of <- function(x) {
    switch(typeof(x),
      logical = ifelse(is.na(x), "NA", ifelse(x, "T", "F")),
      integer = ifelse(is.na(x), "NA", format(x, trim = TRUE, scientific = FALSE)),
      double = double_tokens(x),
      complex = paste0("(", double_tokens(Re(x)), " ", double_tokens(Im(x)), ")"),
      character = vapply(x, string_token, "", USE.NAMES = FALSE),
      raw = as.character(x)
    )
  }

  # A checksum that depends on the position of every value, as two sums of the values weighted
  # by their index under two moduli. Both stay whole numbers below 2^53 up to 60 million
  # doubles, so the order of the additions does not change them.
  weigh <- function(values) {
    values <- as.double(values)
    index <- seq_along(values)
    paste0(sprintf("%.0f", sum(values * (index %% 251 + 1))), "/", sprintf("%.0f", sum(values * (index %% 65521 + 1))))
  }

  # The middle of a long numeric vector: where its NA and NaN are, and the bits of the rest.
  double_summary <- function(x) {
    nan <- is.nan(x)
    missing <- is.na(x) & !nan
    x[nan | missing] <- 0
    paste0("na=", weigh(missing), " nan=", weigh(nan), " bits=", weigh(as.integer(writeBin(x, raw(), endian = "big"))))
  }

  atomic_line <- function(x) {
    x <- as.vector(unclass(x))
    count <- length(x)
    if (count <= .oracle_observe_limit) return(paste(tokens_of(x), collapse = " "))
    head_count <- .oracle_observe_limit %/% 2L
    middle <- x[(head_count + 1L):(count - head_count)]
    summary <- switch(typeof(x),
      character = {
        present <- !is.na(middle)
        bytes <- lapply(middle[present], charToRaw)
        marks <- Encoding(middle[present])
        if (!.oracle_observe_strict) {
          wide <- vapply(bytes, function(text) any(text >= as.raw(0x80)), NA)
          marks[wide & marks %in% c("UTF-8", "unknown") & validUTF8(middle[present])] <- "u"
        }
        paste0("na=", weigh(!present), " lengths=", weigh(lengths(bytes)), " bytes=", weigh(as.integer(unlist(bytes))),
          " marks=", weigh(match(marks, c("unknown", "u", "UTF-8", "latin1", "bytes"))))
      },
      raw = paste0("bytes=", weigh(as.integer(middle))),
      complex = paste0("re ", double_summary(Re(middle)), " im ", double_summary(Im(middle))),
      double_summary(as.double(middle))
    )
    paste(c(tokens_of(x[seq_len(head_count)]), paste0("...[", summary, "]"), tokens_of(x[(count - head_count + 1L):count])), collapse = " ")
  }

  environment_line <- function(env) {
    if (identical(env, globalenv())) return("<global>")
    if (identical(env, baseenv())) return("<base>")
    if (identical(env, emptyenv())) return("<empty>")
    name <- environmentName(env)
    if (nzchar(name)) return(paste0("<", if (isNamespace(env)) "namespace:" else "", name, ">"))
    names <- sort(ls(env, all.names = TRUE), method = "radix")
    paste0("<env", if (environmentIsLocked(env)) " locked", " [", paste(vapply(names, string_token, ""), collapse = " "), "]>")
  }

  walk <- function(x, depth, label) {
    if (depth > 40L) {
      emit(depth, label, "<too deep>")
      return(invisible())
    }
    kind <- typeof(x)
    flags <- paste0(if (is.object(x)) " obj", if (isS4(x)) " S4")
    switch(kind,
      "NULL" = emit(depth, label, "NULL"),
      logical = , integer = , double = , complex = , character = , raw =
        emit(depth, label, kind, "[", length(x), "]", flags, " ", atomic_line(x)),
      list = , expression = {
        emit(depth, label, kind, "[", length(x), "]", flags)
        items <- unclass(x)
        attributes(items) <- NULL
        for (index in seq_along(items)) {
          # A list can hold the empty symbol, which cannot be passed as an argument.
          if (identical(items[[index]], quote(expr = ))) emit(depth + 1L, index, ": symbol <empty>")
          else walk(items[[index]], depth + 1L, paste0(index, ": "))
        }
      },
      symbol = emit(depth, label, "symbol ", if (identical(x, quote(expr = ))) "<empty>" else string_token(as.character(x))),
      language = , pairlist = {
        emit(depth, label, kind, "[", length(x), "]", flags)
        tags <- names(x)
        items <- as.list(unclass(x))
        for (index in seq_along(items)) {
          tag <- if (!is.null(tags) && nzchar(tags[[index]])) paste0(string_token(tags[[index]]), " = ") else ""
          # An empty argument cannot be bound to a variable, so it is tested in place.
          if (identical(items[[index]], quote(expr = ))) emit(depth + 1L, index, ": ", tag, "symbol <empty>")
          else walk(items[[index]], depth + 1L, paste0(index, ": ", tag))
        }
      },
      closure = {
        emit(depth, label, "closure", flags, " in ", environment_line(environment(x)))
        walk(formals(x), depth + 1L, "formals: ")
        walk(body(x), depth + 1L, "body: ")
      },
      builtin = , special = emit(depth, label, kind, " ", paste(deparse(x), collapse = "")),
      environment = emit(depth, label, "environment", flags, " ", environment_line(x)),
      S4 = emit(depth, label, "S4", flags),
      emit(depth, label, kind, flags)
    )
    if (kind %in% c("symbol", "builtin", "special", "NULL")) return(invisible())
    attrs <- attributes(x)
    # Source references hold file positions and an environment, which only say how the code
    # was read.
    attrs <- attrs[!names(attrs) %in% c("srcref", "srcfile", "wholeSrcref", if (kind == "pairlist") "names")]
    for (name in names(attrs)) {
      value <- attrs[[name]]
      if (identical(name, "row.names") && is.data.frame(x)) value <- .row_names_info(x, 0L)
      walk(value, depth + 1L, paste0("@", string_token(name), ": "))
    }
    invisible()
  }

  if (missing(value)) return("symbol <empty>")
  walk(value, 0L, "")
  paste(lines, collapse = "\n")
}

# A condition's kind and its classes, in order. The driver appends the message and the call.
.oracle_observe_condition <- function(kind, condition) {
  paste0(kind, " <", paste(class(condition), collapse = "/"), ">")
}
