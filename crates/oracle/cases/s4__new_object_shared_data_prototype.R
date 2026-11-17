# `R_do_new_object` gives the class attribute and the S4 bit to a copy of the prototype when the
# class comes from a package, except when the prototype is a symbol, an environment or an
# external pointer: each is one shared object, so the object is the bare prototype. The methods
# package's own start-up makes an object of the basic class "name" this way.
t <- function(e) { r <- tryCatch(e, error = function(e) paste("error:", conditionMessage(e))); print(typeof(r)); print(attributes(r)); print(isS4(r)) }
for (k in c("name", "environment", "externalptr", "numeric")) {
    d <- getClass(k)
    d@className <- structure(k, package = "methods")
    t(.Call(methods:::C_new_object, d))
}
t(new("name"))
t(attributes(as.name("<UNDEFINED>")))
setClass("E", contains = "environment")
t(class(new("E")))
setClass("N", contains = "name")
t(new("N"))
t(attributes(quote(x)))
