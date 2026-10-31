# Records native routine calls of corpus packages under GNU R, for ry to replay through the
# same compiled code (crates/libr/tests/test_corpus_calls.rs). Each case names a package, a
# registered .Call routine and its arguments; the recording keeps the arguments and GNU R's
# result, serialized, so ry reads both with its own unserializer. Record in a UTF-8 locale,
# because ry always runs in one.
#
#   LC_ALL=C.UTF-8 R_LIBS=.corpus/library Rscript crates/libr/tests/data/corpus_calls/record.R <out.rds>

cases <- list(
  list("stringi", "C_stri_reverse", list(c("abc", "héllo", NA))),
  list("stringi", "C_stri_length", list(c("a", "ab", "日本", NA))),
  list("stringi", "C_stri_trans_toupper", list(c("abc", "straße"), NULL)),
  list("stringi", "C_stri_count_fixed", list(c("banana", "abc"), "a", NULL)),
  list("stringi", "C_stri_escape_unicode", list("café ☃")),
  list("data.table", "Cchmatch", list(c("b", "z", "a", NA), c("a", "b", "c", NA), 0L)),
  list("jsonlite", "R_parse", list('{"a":[1,2,3],"b":"x","c":null,"d":true}', FALSE)),
  list("jsonlite", "C_escape_chars", list(c("a\"b", "tab\there", "line\nbreak"))),
  list("cli", "clic_ansi_strip", list("\033[31mred\033[39m plain", TRUE, TRUE, TRUE)),
  list("cli", "clic_utf8_display_width", list(c("abc", "日本", "é"))),
  list("cli", "clic_xxhash", list(c("a", "hello world"))),
  list("cli", "clic_xxhash64", list(c("a", "hello world"))),
  list("cli", "clic_ansi_nchar", list(c("\033[1mbold\033[22m", "x"), 1L)),
  list("bit64", "cumsum_integer64", list(structure(as.double(1:5), class = "integer64"),
                                          structure(double(5), class = "integer64"))),
  list("glue", "trim_", list("\n    first\n      second\n    ")),
  list("base64enc", "B64_encode", list(as.raw(0:40), 0L, "\n")),
  list("sodium", "R_crypto_generichash", list(charToRaw("abc"), 32L, NULL)),
  list("xts", "do_is_ordered", list(c(1, 2, 2, 3), TRUE, TRUE)),
  list("xts", "roll_sum", list(as.double(1:10), 3L)),
  list("zoo", "zoo_lag", list(structure(as.double(1:5), class = "zoo", index = 1:5), 1L, FALSE)),
  list("utf8", "rutf8_utf8_normalize", list(c("Café", "Å"), TRUE, FALSE, FALSE, FALSE)),
  list("digest", "digest_impl", list(charToRaw("abc"), 1L, -1L, 0L, 0L, 0L, NULL)),
  list("rlang", "ffi_hash", list(1:10, TRUE))
)

args <- commandArgs(trailingOnly = TRUE)
out <- if (length(args)) args[[1]] else "cases.rds"
recorded <- list()
for (case in cases) {
  package <- case[[1]]
  name <- case[[2]]
  arguments <- case[[3]]
  namespace <- asNamespace(package)
  # A routine the namespace does not bind as a symbol object is looked up by name in the
  # package's library.
  symbol <- tryCatch(get(name, envir = namespace), error = function(error) NULL)
  if (!inherits(symbol, "NativeSymbolInfo")) {
    dll <- namespace$.__NAMESPACE__.$DLLs[[1]]
    symbol <- tryCatch(getNativeSymbolInfo(name, dll), error = function(error) NULL)
    if (is.null(symbol)) {
      message(sprintf("skipped %s::%s: no such routine", package, name))
      next
    }
  }
  dll <- symbol$dll[["name"]]
  result <- tryCatch(
    do.call(.Call, c(list(symbol), arguments)),
    error = function(error) structure(conditionMessage(error), class = "recorded-error")
  )
  if (inherits(result, "recorded-error")) {
    message(sprintf("skipped %s::%s: %s", package, name, result))
    next
  }
  routine <- symbol$name
  recorded[[length(recorded) + 1L]] <- list(
    package = package, dll = dll, routine = routine, arguments = arguments, result = result
  )
}
saveRDS(recorded, out, version = 3, compress = FALSE)
message(sprintf("recorded %d of %d cases", length(recorded), length(cases)))
