# getParseData() returns GNU R's token table for a parse with srcrefs kept. The ids are the
# parser's internal node numbers, so the case leaves them out.
p <- parse(text = "f <- function(x, y = 2) {\n  x + y # sum\n}\nf(1)", keep.source = TRUE)
d <- getParseData(p)
print(d[, c("line1", "col1", "line2", "col2", "token", "terminal", "text")], row.names = FALSE)
getParseText(d, d$id[d$token == "FUNCTION"])
nrow(getParseData(parse(text = "x[[1]]$a@b", keep.source = TRUE)))

# t() keeps attributes other than dim and dimnames.
m <- structure(matrix(1:6, 2), extra = "kept", dimnames = list(c("a", "b"), NULL))
attributes(t(m))
