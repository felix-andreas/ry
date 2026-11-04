# A top-level `#ifdef` may hold whole sections.
rd <- tools::parse_Rd(textConnection("\\name{x}\n#ifdef unix\n\\section{Unix}{text}\n#endif\n\\title{T}\n"))
print(vapply(rd, function(node) attr(node, "Rd_tag"), ""))
print(vapply(rd[[3]][[2]], function(node) attr(node, "Rd_tag"), ""))
# An unknown macro name is read again without its trailing digits, and `\linkS4class` takes an
# option like `\link`.
rd <- tools::parse_Rd(textConnection("0\\dots6 \\linkS4class[pkg]{cls}"), fragment = TRUE)
print(vapply(rd, function(node) attr(node, "Rd_tag"), ""))
# An unknown macro is a lexer warning, so it does not mention the earlier multi-line string.
text <- "\\code{x <- \"a\nb\"} \\nosuchmacro{y} \\dontdiff{z}"
rd <- tools::parse_Rd(textConnection(text), fragment = TRUE)
print(vapply(rd, function(node) attr(node, "Rd_tag"), ""))
