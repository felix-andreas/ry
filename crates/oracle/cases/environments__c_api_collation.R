# The C API orders strings with the session's collation, as sort() does: R_orderVector1 and
# Rf_isUnsorted follow ICU's order under C.UTF-8 and byte order under C.
source_file <- file.path(tempdir(), "collation_probe.c")
writeLines(c(
  "#include <Rinternals.h>",
  "SEXP order_strings(SEXP x, SEXP decreasing) {",
  "  int n = LENGTH(x);",
  "  SEXP result = PROTECT(allocVector(INTSXP, n));",
  "  R_orderVector1(INTEGER(result), n, x, TRUE, asLogical(decreasing));",
  "  for (int i = 0; i < n; i++) INTEGER(result)[i] += 1;",
  "  UNPROTECT(1);",
  "  return result;",
  "}",
  "SEXP unsorted(SEXP x) { return ScalarLogical(Rf_isUnsorted(x, FALSE)); }"
), source_file)
status <- system2(file.path(R.home("bin"), "R"), c("CMD", "SHLIB", shQuote(source_file)), stdout = FALSE, stderr = FALSE)
print(status)
library_file <- sub("\\.c$", .Platform$dynlib.ext, source_file)
dll <- dyn.load(library_file)
words <- c("b", "B", "_x", "a", "A", "é", "f", "É", "ab", "a.b", "a_b", "Zeta", "zeta", NA)
for (locale in c("C.UTF-8", "C", "C.UTF-8")) {
  invisible(Sys.setlocale("LC_COLLATE", locale))
  cat(locale, "\n")
  order <- .Call(getNativeSymbolInfo("order_strings", dll), words, FALSE)
  print(words[order])
  print(identical(order, order(words)))
  print(words[.Call(getNativeSymbolInfo("order_strings", dll), words, TRUE)])
  print(.Call(getNativeSymbolInfo("unsorted", dll), c("a", "B", "c")))
  print(.Call(getNativeSymbolInfo("unsorted", dll), c("B", "a", "c")))
}
dyn.unload(library_file)
