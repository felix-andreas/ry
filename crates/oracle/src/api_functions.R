# Writes the table of base, stats and utils functions behind the `api` fuzz kind and the fuzz
# coverage report: one line per function with its package, its name, `function` or `method` (an
# S3 method), how many leading arguments a call may pass by position (`skip` for a function the
# fuzzer never calls), and its formals, where `!` marks one a call never passes. Run it under
# the reference GNU R and store the output next to this file:
#   Rscript crates/oracle/src/api_functions.R > crates/oracle/src/api_functions.txt
#
# A function is skipped when a call could differ between two correct engines or reach outside
# the process: files, connections, the clock, the session, the garbage collector, native code,
# graphics devices, build information, and loops that need not end.
skipped <- list(
  base = c(
    "^Sys\\.", "^sys\\.source$", "^sys\\.(save|load)\\.image$", "^file", "^dir\\.", "^list\\.(files|dirs)$",
    "^unlink$", "^system", "^shell", "[cC]onnection", "^socket", "^serverSocket$", "^url$",
    "^gzcon$", "^(gz|bz|xz|zstd)file$", "^pipe$", "^fifo$", "^unz$", "^open", "^close", "^sink",
    "^read", "^write", "^scan$", "^save", "^load", "^source$", "^dump$", "^dget$",
    "^std(in|out|err)$", "^isatty$", "^flush", "^seek", "^truncate", "^pushBack", "^clearPushBack$",
    "^nsl$", "^curlGetHeaders$", "^isOpen$", "^isIncomplete$", "^isSeekable$", "^mem", "^gc",
    "^debug", "^undebug$", "^isdebugged$", "^browser", "^trac", "^untrace", "^retracemem$",
    "^dyn\\.", "^library", "^require", "[nN]amespace", "^importIntoEnv$", "^attach", "^detach$",
    "^search", "^conflicts$",
    "^(q|quit|readline|proc\\.time|unix\\.time|date|rm|remove|setwd|getwd|tempfile|tempdir|on\\.exit|options)$",
    "^set(Session)?TimeLimit$", "^(while|repeat|for)$", "^commandArgs$", "^extSoftVersion$",
    "^La_", "^libcurlVersion$", "^pcre_config$", "^icu", "^capabilities$", "^l10n_info$",
    "^R\\.", "^R_compiled_by$", "^Cstack_info$", "^getLoadedDLLs$", "^getNativeSymbolInfo$",
    "^is\\.loaded$", "^getDLLRegisteredRoutines", "^getCallingDLL", "^lock", "^unlock",
    "^reg\\.finalizer$", "^autoload", "^Autoloads$", "^builtins$", "^bindtextdomain$",
    "^gettext$", "^ngettext$", "^Recall$", "^invokeRestartInteractively$",
    "^globalCallingHandlers$", "^taskCallback", "TaskCallback", "^srcfile", "^srcref$",
    "^getSrcLines$", "^packageStartupMessage$", "^packageEvent$", "^setHook$", "^getHook$",
    "^userhooks", "^print\\.function$", "^Ops\\.", "^Math\\.", "^Summary\\.", "^withAutoprint$",
    "warnings$", "^lazyLoad", "^makeLazyLoad", "^registerS3", "^normalizePath$", "^path\\.expand$",
    "^plot$", "^contributors$", "^licen[cs]e$", "^nullfile$", "^memCompress$", "^memDecompress$"
  ),
  stats = c(
    "plot", "^lines", "^points", "^text\\.", "^identify", "^rect\\.hclust$", "^scatter\\.smooth$",
    "^heatmap$", "^screeplot", "^(acf|pacf|ccf|cpgram|spectrum|spec\\.pgram|spec\\.ar|spec)$",
    "^qq(norm|line)", "^(read|write)\\.ftable$", "^monthplot", "^Pair$"
  )
)
kept_internals <- c(
  ".subset", ".subset2", ".row_names_info", ".set_row_names", ".mapply", ".colSums", ".rowSums",
  ".colMeans", ".rowMeans", ".bincode", ".POSIXct", ".POSIXlt", ".Date", ".difftime", ".class2",
  ".kronecker", ".make_numeric_version", ".encode_numeric_version", ".decode_numeric_version",
  ".format.zeros", ".deparseOpts", ".col", ".row", ".valid.factor", ".rangeNum"
)
# `adist` is left out: with `counts = TRUE` and an NA string GNU R 4.6.2 writes past a buffer
# and aborts, so its answer is no reference.
kept_utils <- c(
  "head", "tail", "head.matrix", "tail.matrix", "modifyList", "combn", "stack", "unstack",
  "type.convert", "str", "hasName", "relist", "as.relistable", "glob2rx", "strcapture",
  "aregexec", "compareVersion", "formatUL", "formatOL", "as.roman", "charClass", "URLencode",
  "URLdecode", "capture.output", "isS3method", "isS3stdGeneric", "write.table", "write.csv",
  "write.csv2", "strOptions", "toBibtex", "warnErrList", "getS3method", "packageVersion",
  "as.person", "person", "bitwAnd", "txtProgressBar", "setTxtProgressBar", "getTxtProgressBar",
  "localeToCharset", "limitedLabels", "is.relistable", "hashtab", "gethash", "sethash",
  "remhash", "numhash", "typhash", "maphash", "clrhash", "is.hashtab", "format.object_size",
  "tail.default", "head.default", "trimws", "formatList", "nsl", "citeNatbib", "isatty"
)
# A formal that names a file or a library: a call passes arguments by position only before it.
outside <- c("file", "con", "description", "filename", "path", "paths", "dir", "command", "package", "pkg", "lib.loc", "zipfile", "tarfile")
path_text <- c("basename", "dirname")
for (package in c("base", "stats", "utils")) {
  env <- asNamespace(package)
  exported <- if (package == "base") ls(env, all.names = TRUE) else getNamespaceExports(package)
  for (name in sort(exported, method = "radix")) {
    f <- get(name, envir = env)
    if (!is.function(f)) next
    a <- if (is.primitive(f)) args(f) else f
    parameters <- if (is.null(a)) character() else names(formals(a))
    dots <- match("...", parameters, nomatch = 0L)
    barrier <- if (name %in% path_text) length(parameters) else match(TRUE, parameters %in% outside, nomatch = length(parameters) + 1L) - 1L
    positional <- if (is.primitive(f) && is.null(a)) {
      3L
    } else if (dots > 0L && dots - 1L <= barrier) {
      3L
    } else {
      min(barrier, length(parameters), 3L)
    }
    skip <- (startsWith(name, ".") && !name %in% kept_internals) ||
      any(vapply(skipped[[package]], grepl, NA, name)) ||
      (package == "utils" && !name %in% kept_utils) ||
      (positional == 0L && length(parameters) > 0L)
    if (!name %in% path_text) parameters[parameters %in% outside] <- paste0(parameters[parameters %in% outside], "!")
    method <- !is.primitive(f) && isTRUE(utils::isS3method(name, envir = env))
    cat(package, name, if (method) "method" else "function", if (skip) "skip" else positional, paste(parameters, collapse = ","), sep = "\t")
    cat("\n")
  }
}
