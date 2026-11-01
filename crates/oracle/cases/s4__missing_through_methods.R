# A method's formal whose argument the generic's call left out stays missing when the method
# passes it on, though the method's own default gives it a value.
setGeneric("ap", function(x, unit, ...) standardGeneric("ap"))
setMethod("ap", "numeric", function(x, unit = "second", ...) missing(unit))
setMethod("ap", "logical", function(x, unit = NULL, ...) ap(as.numeric(x), unit, ...))
ap(NA)
ap(NA, "minute")

# A plain closure's default is not missing for the function it is passed to.
g <- function(u) missing(u)
f <- function(u = NULL) g(u)
f()

# A generic's own default is not missing either.
setGeneric("bp", function(x, unit = "hour") standardGeneric("bp"))
setMethod("bp", "numeric", function(x, unit = "hour") g(unit))
bp(1)
