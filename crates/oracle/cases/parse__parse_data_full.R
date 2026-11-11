# getParseData() gives GNU R's exact table, ids and parents included, for comments at every
# level, `if`/`else` split across lines inside braces, named and empty arguments, `...`, `[[`,
# formulas, lambdas, pipes, right assignment, `?`, tabs, `;` and a `#:` comment that runs on
# from the end of a statement over the next line. Every srcref of the parse matches too.
src <- c(
  "# leading comment",
  "f <- function(x, ..., y = 2) { # opens",
  "  # inside",
  "  if (x > 1) {",
  "    g(... = x, , y)",
  "  }",
  "  else if (y) x[[1]]$a@b else NULL",
  "  z <- ~ x + y; w = \\(v) v |> h()",
  "\tx -> q; ?help",
  "}",
  "",
  "f(1)[2] # trailing",
  "h <- g(1) #: (x: Int) -> Int",
  "  #: a second annotation line",
  "",
  "# between",
  "lst <- list(NULL = 1, 'a' = 2, ..1 = 3)",
  "repeat break; while (TRUE) next",
  "for (i in seq_len(3)) {",
  "",
  "  -i # negated",
  "}",
  "# last"
)
p <- parse(text = src, keep.source = TRUE)
print(getParseData(p), row.names = FALSE)
for (ref in attr(p, "srcref")) print(as.integer(ref))
print(as.integer(attr(p, "wholeSrcref")))
for (ref in attr(p[[1]][[3]][[3]], "srcref")) print(as.integer(ref))
