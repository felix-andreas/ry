# An S4 object whose class is no longer defined prints its attributes, but deparse, dput and
# dump raise GNU R's error, because the deparser reads the slots from the missing definition.
setClass("Zzz", representation(x = "numeric"))
z <- new("Zzz", x = 1)
removeClass("Zzz")
z
tryCatch(deparse(z), error = conditionMessage)
tryCatch(dput(z), error = conditionMessage)
tryCatch(dump("z", stdout()), error = conditionMessage)
cat(tryCatch(stop("in a message: ", deparse(list(z))), error = conditionMessage), "\n")
str(z)
tryCatch(as.character(list(z)), error = conditionMessage)
tryCatch(paste(list(z)), error = conditionMessage)
# getClassDef's own error, for a class whose package does not exist, reaches the handler.
x <- asS4(structure(1, class = structure("foo", package = "nopkgzz")))
suppressMessages(suppressWarnings(tryCatch(deparse(x), error = function(e) "caught")))
# A slot the definition names but the object lacks is the slot read's error.
setClass("Two", representation(x = "numeric", y = "numeric"))
w <- new("Two", x = 1, y = 2)
attr(w, "y") <- NULL
tryCatch(deparse(w), error = conditionMessage)
