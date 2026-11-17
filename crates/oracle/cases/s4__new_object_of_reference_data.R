# The C-level constructor leaves a symbol, an environment and an external pointer unmarked: such a
# prototype is the object itself, not a copy, so a class set on it would mark every use of it.
show <- function(class) {
    object <- .Call(methods:::C_new_object, getClass(class))
    print(typeof(object))
    print(names(attributes(object)))
    print(isS4(object))
    invisible(object)
}
undefined <- show("name")
print(undefined)
print(attributes(quote(`<UNDEFINED>`)))
show("environment")
show("externalptr")
# A class with ordinary data is copied and marked.
setClass("Marked", representation(value = "numeric"))
marked <- show("Marked")
print(is(marked, "Marked"))
# The relation the methods package sets up while it is built.
print(is(quote(x), "language"))
print(is(as.name("x"), "name"))
print(extends("name", "language"))
setClass("Wrapper", contains = "name")
print(isVirtualClass("Wrapper"))
wrapped <- new("Wrapper", as.name("y"))
print(typeof(wrapped))
print(is(wrapped, "language"))
