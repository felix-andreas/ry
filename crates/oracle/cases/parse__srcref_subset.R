# `[` on a parsed expression subsets its srcref list with it.
exprs <- parse(text = c("a <- 1", "", "f <- function() 2", "g()"), keep.source = TRUE)
part <- exprs[2:3]
print(length(attr(part, "srcref")))
print(vapply(attr(part, "srcref"), function(ref) ref[[1]], 0L))
print(as.character(attr(exprs[3], "srcref")[[1]]))
print(is.null(attr(exprs[], "srcref")))
