# rapply(how = "replace") keeps every list's attributes; how = "list" keeps only names.
x <- structure(list(a = 1, b = structure(list(c = "x"), class = "inner"), d = "y"), class = "outer", extra = 5)
str(rapply(x, function(v) v, how = "replace"))
str(rapply(x, toupper, classes = "character", how = "replace"))
str(rapply(x, function(v) v, how = "list"))
str(unlist(list(list(x), list(x)), recursive = FALSE))
