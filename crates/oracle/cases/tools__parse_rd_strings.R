# R strings inside R-like Rd text: outside a string `\"` is a plain quote that opens nothing, and
# inside one an escaped quote never closes it. A string may span a closing brace and a newline,
# and the newline is only mentioned alongside a syntax error.
show <- function(text) {
  rd <- tools::parse_Rd(textConnection(text), fragment = TRUE)
  print(paste(as.character(rd, deparse = TRUE), collapse = ""))
  print(vapply(rd, function(node) attr(node, "Rd_tag"), ""))
}
show("\\code{rootpath=\"\\\"} relative to \\code{\"\\\"}")
show("\\code{x <- \"a\nb\"} and \\code{'\\''}")
show("\\code{\"a\\\\\\\"b\"} \\code{\\\"}")
show("\\code{f(\"\\link{g}\")}")
