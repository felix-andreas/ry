# A body changed in place through the C API after it compiled. A closure that already runs the
# compiled code keeps it, while a closure made from the changed body runs the change, at the top
# level of the body and in a nested call alike, as GNU R's JIT cache behaves.
source_file <- file.path(tempdir(), "poke_node.c")
writeLines(c(
  "#include <Rinternals.h>",
  "SEXP poke_node(SEXP expression, SEXP path, SEXP value) {",
  "  SEXP node = expression;",
  "  int depth = LENGTH(path);",
  "  for (int k = 0; k < depth; k++) {",
  "    for (int j = 1; j < INTEGER(path)[k]; j++) node = CDR(node);",
  "    if (k < depth - 1) node = CAR(node);",
  "  }",
  "  SETCAR(node, value);",
  "  return R_NilValue;",
  "}"
), source_file)
status <- system2(file.path(R.home("bin"), "R"), c("CMD", "SHLIB", shQuote(source_file)), stdout = FALSE, stderr = FALSE)
print(status)
library_file <- sub("\\.c$", .Platform$dynlib.ext, source_file)
dll <- dyn.load(library_file)
poke <- function(expression, path, value) {
  invisible(.Call(getNativeSymbolInfo("poke_node", dll), expression, as.integer(path), value))
}

maker <- quote(function(n) {
  total <- 0
  for (i in seq_len(n)) total <- total + i
  total
})
first <- eval(maker)
print(c(first(4), first(4), eval(maker)(4)))
poke(maker[[3]], 2, quote(total <- 100))
second <- eval(maker)
print(c(first(4), second(4), second(4)))
print(body(second)[[2]])
poke(maker[[3]], c(3, 4, 3, 3), quote(i * 2))
third <- eval(maker)
print(c(first(4), second(4), third(4), third(4)))
print(body(third)[[3]])
dyn.unload(library_file)
