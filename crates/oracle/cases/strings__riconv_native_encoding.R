# Riconv_open reads "" as the codeset of the current LC_CTYPE, as iconv_open does, so the same
# bytes convert under C.UTF-8, fail under C (ASCII), and read as Latin-1 under a Latin-1
# locale; lowercase "utf-8" names UTF-8 too. The Latin-1 block needs that locale generated
# (`localedef -i en_US -f ISO-8859-1 en_US.ISO-8859-1`, which `tools/dev-setup.sh` runs).
# oracle: requires locale en_US.ISO-8859-1
source_file <- file.path(tempdir(), "riconv_probe.c")
writeLines(c(
  "#include <errno.h>",
  "#include <string.h>",
  "#include <Rinternals.h>",
  "#include <R_ext/Riconv.h>",
  "SEXP convert(SEXP to, SEXP from, SEXP input) {",
  "  void *cd = Riconv_open(CHAR(STRING_ELT(to, 0)), CHAR(STRING_ELT(from, 0)));",
  "  if (cd == (void *) -1) return mkString(\"open failed\");",
  "  const char *in = (const char *) RAW(input);",
  "  size_t in_left = XLENGTH(input);",
  "  char buffer[64];",
  "  char *out = buffer;",
  "  size_t out_left = sizeof buffer;",
  "  size_t status = Riconv(cd, &in, &in_left, &out, &out_left);",
  "  int error = errno;",
  "  Riconv_close(cd);",
  "  if (status == (size_t) -1) return mkString(error == EILSEQ ? \"invalid input\" : \"failed\");",
  "  SEXP result = PROTECT(allocVector(RAWSXP, out - buffer));",
  "  memcpy(RAW(result), buffer, out - buffer);",
  "  UNPROTECT(1);",
  "  return result;",
  "}"
), source_file)
status <- system2(file.path(R.home("bin"), "R"), c("CMD", "SHLIB", shQuote(source_file)), stdout = FALSE, stderr = FALSE)
print(status)
library_file <- sub("\\.c$", .Platform$dynlib.ext, source_file)
dll <- dyn.load(library_file)
convert <- function(to, from, bytes) .Call(getNativeSymbolInfo("convert", dll), to, from, bytes)
e_acute <- as.raw(c(0xc3, 0xa9))
print(convert("utf-8", "utf-8", e_acute))
print(convert("UTF-8", "latin1", as.raw(0xe9)))
for (locale in c("C.UTF-8", "C", "en_US.ISO-8859-1", "C.UTF-8")) {
  stopifnot(nzchar(Sys.setlocale("LC_CTYPE", locale)))
  cat(locale, "\n")
  print(convert("", "", e_acute))
  print(convert("UTF-8", "", e_acute))
  print(convert("UTF-8", "", as.raw(0xe9)))
  print(convert("", "UTF-8", charToRaw("abc")))
}
dyn.unload(library_file)
