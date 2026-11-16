# exists() reports an active binding without calling its function; a mode other than "any"
# needs the value, so it does call it.
e <- new.env()
makeActiveBinding("x", function(value) stop("the binding was called"), e)
print(exists("x", envir = e, inherits = FALSE))
print(exists("x", envir = e))
print(exists("y", envir = e, inherits = FALSE))
attach(e, name = ".active")
print(exists("x", ".active", inherits = FALSE))
detach(".active")
print(tryCatch(exists("x", envir = e, mode = "function"), error = function(c) conditionMessage(c)))
