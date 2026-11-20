# read.dcf() leaves every value in the native encoding, so desc's check that the values match
# read.dcf(all = TRUE)'s holds for a DESCRIPTION with a non-ASCII author, and a latin1 byte
# survives the read.
lines <- c("Package: p", "Author: Kirill M\xc3\xbcller [aut]", "Note: caf\xe9", "Encoding: UTF-8")
read <- function(...) {
  con <- textConnection(lines, local = TRUE)
  on.exit(close(con))
  read.dcf(con, ...)
}
kept <- c("Package", "Author", "Note", "Encoding")
plain <- read(keep.white = kept)
all <- read(keep.white = kept, all = TRUE)
print(Encoding(plain))
print(Encoding(unlist(all)))
print(plain != all)
print(charToRaw(plain[, "Note"]))
print(charToRaw(plain[, "Author"]))
