# parse(file, keep.source = TRUE) gives GNU R's exact parse data and srcrefs for files with
# non-ASCII strings and comments, tabs before tokens, `#:` comments, CRLF line ends, `if`/`else`
# split across lines inside braces, an empty file and a file that does not parse. GNU R's srcref
# byte fields count one byte too many for each multibyte character of a file read as UTF-8, which
# ry does not reproduce, so the non-ASCII file shows its parse data only.
show <- function(lines, eol = "\n", srcrefs = TRUE) {
  path <- tempfile(fileext = ".R")
  on.exit(unlink(path))
  writeBin(charToRaw(enc2utf8(paste0(lines, eol, collapse = ""))), path)
  parsed <- tryCatch(
    parse(path, keep.source = TRUE, encoding = "UTF-8"),
    error = function(e) {
      cat("error:", sub("^[^:]*:", "", conditionMessage(e)), "\n")
      NULL
    }
  )
  if (is.null(parsed)) return(invisible())
  cat("expressions:", length(parsed), "\n")
  data <- getParseData(parsed)
  if (!is.null(data)) print(data, row.names = FALSE)
  if (srcrefs) for (ref in attr(parsed, "srcref")) print(as.integer(ref))
}

show(c(
  "s <- \"café über\" # grüße",
  "\tx <-\t1 +\t2",
  "  t <- 'naïve été'; y <- \tt # été"
), srcrefs = FALSE)

show(c(
  "#: (x: Int) -> Int",
  "f <- function(x) {",
  "  #: a note inside",
  "  x + 1 #: trailing annotation",
  "}",
  "g <- f(2) #: Int"
))

show(c(
  "a <- 1",
  "if (a > 0) {",
  "  b <- 2",
  "} else {",
  "  b <- 3",
  "}",
  "# done"
), eol = "\r\n")

show(c(
  "h <- function(x) {",
  "  if (x)",
  "    1",
  "  else",
  "    2",
  "  if (x) {",
  "    3",
  "  }",
  "  else if (!x) 4 else",
  "    5",
  "}"
))

show(character())

show(c("x <- 1", "y <- (2 +", "z <- 3"))
