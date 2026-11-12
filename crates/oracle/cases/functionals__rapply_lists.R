# rapply(how = "replace") keeps every list's attributes; how = "list" keeps only names.
x <- structure(list(a = 1, b = structure(list(c = "x"), class = "inner"), d = "y"), class = "outer", extra = 5)
str(rapply(x, function(v) v, how = "replace"))
str(rapply(x, toupper, classes = "character", how = "replace"))
str(rapply(x, function(v) v, how = "list"))
str(unlist(list(list(x), list(x)), recursive = FALSE))
# A NULL element is walked as an empty list, so FUN never sees it.
str(rapply(list(a = 1, b = NULL), function(x) "called", how = "list"))
str(rapply(list(a = 1, b = NULL), function(x) "called", how = "replace"))
str(rapply(list(a = 1, b = NULL), function(x) "called", how = "unlist"))
str(rapply(list(a = 1, b = NULL), function(x) "called", classes = "numeric", deflt = "d", how = "list"))
