# rapply checks its arguments as GNU R does, walks expression vectors as it walks lists, and matches
# `classes` against what class() reports.
t <- function(e) tryCatch(e, error = function(e) paste("error:", conditionMessage(e)))
t(rapply(list(1, "a"), function(x) x, 1))
t(rapply(list(1, "a"), function(x) x, list("numeric")))
t(rapply(list(1, "a"), function(x) x, NULL))
t(rapply(list(1, "a"), function(x) x, c(TRUE, NA)))
t(rapply(list(1, "a"), function(x) x, quote(numeric), how = "list"))
t(rapply(list(1, "a"), function(x) x, "numeric", how = 1))
t(rapply(1:3, function(x) x))
t(rapply(NULL, function(x) x))
t(rapply(list(1, "a"), 1))
t(rapply(list(1, "a"), "nchar", "character", deflt = NULL, how = "list"))
str(rapply(list(1, "a"), function(x) x, NA_character_, how = "list"))
str(rapply(expression(a, 1), function(x) x * 2, "numeric", how = "list"))
rapply(expression(a, 1), function(x) x * 2, "numeric", how = "replace")
str(rapply(expression(a, 1, b = "s"), function(x) class(x), how = "unlist"))
str(rapply(list(expression(a, 1)), function(x) class(x), how = "list"))
str(rapply(list(1, list(2L, quote(x))), function(x) x * 2, c("numeric", "integer"),
           deflt = quote(d), how = "list"))
str(rapply(list(2L, 1.5, matrix(1:4, 2), sum, "s"), function(x) "hit", "numeric",
           deflt = "miss", how = "list"))
str(rapply(list(2L, 1.5, matrix(1:4, 2), sum, "s"), function(x) "hit", c("function", "matrix"),
           deflt = "miss", how = "unlist"))
str(rapply(list(data.frame(a = 1:2, b = c("x", "y")), 3), function(x) class(x), how = "unlist"))
str(rapply(data.frame(a = 1:2, b = c("x", "y")), function(x) rev(x), how = "replace"))
str(rapply(data.frame(a = 1:2, b = c("x", "y")), function(x) x[1], how = "unlist"))
str(rapply(structure(list(a = 1, b = list(c = "s")), class = "k", extra = TRUE),
           toupper, "character", how = "replace"))
str(rapply(list(1, "a"), function(x) "hit", c("numeric", "ANY"), deflt = "miss", how = "list"))
