# Riconv from UTF-8 to UTF-8 must stop as glibc's iconv does: EILSEQ at an invalid sequence,
# EINVAL at a sequence the input cuts off, and E2BIG at the last whole character that fits, with
# the input and output positions and the bytes written so far. glibc also accepts forms strict
# UTF-8 rejects, such as code points past U+10FFFF.
source_file <- file.path(tempdir(), "riconv_probe.c")
writeLines(c(
  "#include <errno.h>",
  "#include <string.h>",
  "#include <Rinternals.h>",
  "#include <R_ext/Riconv.h>",
  "SEXP riconv_probe(SEXP to, SEXP from, SEXP input, SEXP capacity) {",
  "  void *converter = Riconv_open(CHAR(STRING_ELT(to, 0)), CHAR(STRING_ELT(from, 0)));",
  "  size_t room = (size_t) INTEGER(capacity)[0], input_left = (size_t) XLENGTH(input), output_left = room;",
  "  char buffer[256];",
  "  const char *in = (const char *) RAW(input);",
  "  char *out = buffer;",
  "  errno = 0;",
  "  size_t status = Riconv(converter, &in, &input_left, &out, &output_left);",
  "  int error = status == (size_t) -1 ? errno : 0;",
  "  Riconv_close(converter);",
  "  SEXP result = PROTECT(allocVector(VECSXP, 4));",
  "  const char *name = error == EILSEQ ? \"EILSEQ\" : error == EINVAL ? \"EINVAL\" : error == E2BIG ? \"E2BIG\" : error ? \"other\" : \"ok\";",
  "  SET_VECTOR_ELT(result, 0, mkString(name));",
  "  SET_VECTOR_ELT(result, 1, ScalarInteger((int) (XLENGTH(input) - input_left)));",
  "  SET_VECTOR_ELT(result, 2, ScalarInteger((int) (room - output_left)));",
  "  SEXP written = allocVector(RAWSXP, (R_xlen_t) (room - output_left));",
  "  SET_VECTOR_ELT(result, 3, written);",
  "  memcpy(RAW(written), buffer, room - output_left);",
  "  UNPROTECT(1);",
  "  return result;",
  "}"
), source_file)
status <- system2(file.path(R.home("bin"), "R"), c("CMD", "SHLIB", shQuote(source_file)), stdout = FALSE, stderr = FALSE)
print(status)
library_file <- sub("\\.c$", .Platform$dynlib.ext, source_file)
dll <- dyn.load(library_file)
probe <- function(bytes, capacity, from = "UTF-8", to = "UTF-8") {
  result <- .Call(getNativeSymbolInfo("riconv_probe", dll), to, from, as.raw(bytes), as.integer(capacity))
  cat(sprintf("%-6s used %2d wrote %2d: %s\n", result[[1]], result[[2]], result[[3]],
              paste(format(result[[4]]), collapse = " ")))
}
text <- c(charToRaw("caf"), 0xc3, 0xa9, charToRaw(" ok"))
probe(text, 64)
probe(text, 64, from = "")
probe(text, 64, from = "utf8")
probe(c(charToRaw("ab"), 0xff, charToRaw("cd")), 64)
probe(c(charToRaw("ab"), 0xed, 0xa0, 0x80, charToRaw("cd")), 64)
probe(c(charToRaw("ab"), 0xc0, 0x80), 64)
probe(c(charToRaw("ab"), 0xe2, 0x82), 64)
probe(c(charToRaw("ab"), 0xe0, 0x80), 64)
probe(c(charToRaw("ab"), 0xf0), 64)
probe(text, 4)
probe(text, 5)
probe(text, 3)
probe(text, 0)
probe(c(charToRaw("ab"), 0xff), 2)
probe(c(charToRaw("ab"), 0xf4, 0x90, 0x80, 0x80, charToRaw("z")), 64)
probe(c(charToRaw("ab"), 0xf8, 0x88, 0x80, 0x80, 0x80, charToRaw("z")), 64)
probe(c(0xef, 0xbb, 0xbf, charToRaw("bom")), 64)
probe(raw(0), 8)
# Another encoding on either side converts through iconv.
probe(c(charToRaw("caf"), 0xe9), 64, from = "latin1")
probe(text, 64, to = "latin1")
probe(c(charToRaw("ab"), 0xe2, 0x82, 0xac), 64, to = "latin1")
probe(text, 64, to = "UTF-16LE")
probe(c(charToRaw("ab"), 0xff), 64, from = "UTF-8", to = "UTF-16LE")
dyn.unload(library_file)
