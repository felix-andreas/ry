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
