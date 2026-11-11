# oracle: requires rlang
# A compiled call of a safe base internal (`match`) must reach a definition that C code binds in
# the global environment after the site was compiled: through `Rf_defineVar`, `Rf_setVar` and
# `R_MakeActiveBinding`. rlang's `env_bind` calls `Rf_defineVar` and its `env_bind_active` calls
# `R_MakeActiveBinding`; the `Rf_setVar` routine is compiled here. `Rf_setVar` changes the
# nearest existing binding, so it replaces a global definition rather than adding one.
finds <- function(xs) { out <- integer(length(xs)); for (k in seq_along(xs)) out[k] <- match(xs[k], c("a", "b", "c")); out }
for (round in 1:3) print(finds(c("b", "z")))
rlang::env_bind(globalenv(), match = function(x, table, ...) -1L)
print(finds(c("b", "z")))
rm(match, envir = globalenv())
print(finds(c("b", "z")))
rlang::env_bind_active(globalenv(), match = function() function(x, table, ...) -2L)
print(finds(c("b", "z")))
rm(match, envir = globalenv())
print(finds(c("b", "z")))
source_file <- file.path(tempdir(), "set_var.c")
writeLines(c(
  "#include <Rinternals.h>",
  "SEXP set_var(SEXP name, SEXP value, SEXP env) {",
  "  Rf_setVar(Rf_installChar(STRING_ELT(name, 0)), value, env);",
  "  return R_NilValue;",
  "}"
), source_file)
status <- system2(file.path(R.home("bin"), "R"), c("CMD", "SHLIB", shQuote(source_file)), stdout = FALSE, stderr = FALSE)
print(status)
library_file <- sub("\\.c$", .Platform$dynlib.ext, source_file)
dll <- dyn.load(library_file)
# `Rf_setVar` finds the nearest binding, so for `match` it would reach base's locked one: it
# replaces a global definition the sites have already called.
match <- function(x, table, ...) -3L
print(finds(c("b", "z")))
invisible(.Call(getNativeSymbolInfo("set_var", dll), "match", function(x, table, ...) -4L, new.env()))
print(finds(c("b", "z")))
rm(match, envir = globalenv())
print(finds(c("b", "z")))
dyn.unload(library_file)
