# Class definitions deparse with the slots of their own class, `classRepresentation`, not those
# of the class they describe. A definition whose class extends a basic type once recursed until
# the stack ran out, because the failing slot read's error message deparsed the same object.
setClass("A", representation(x = "numeric"))
setClass("N", contains = "numeric")
a <- deparse(getClass("A"))
length(a)
a[1]
n <- deparse(getClass("N"))
length(n)
n[1]
grep("prototype", n, value = TRUE)
deparse(getClass("N")@prototype)
length(deparse(getClass("N"), control = c("keepNA", "keepInteger", "showAttributes", "niceNames", "digits17")))
deparse(new("N", 1:2 + 0.5))
