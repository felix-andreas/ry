# cacheMetaData() on three namespace shapes: one with only old classes (setOldClass with a
# prototype asks for the Ops group to be cached), one with its own generics and methods, and one
# with methods for group generics. Each is cached twice, as a load after an unload does, and the
# dispatch and the methods tables are compared afterwards.
make_ns <- function(name) {
  info <- new.env(hash = TRUE, parent = baseenv())
  imports <- new.env(hash = TRUE, parent = .BaseNamespaceEnv)
  attr(imports, "name") <- paste0("imports:", name)
  ns <- new.env(hash = TRUE, parent = imports)
  assign(".__NAMESPACE__.", info, envir = ns)
  info$spec <- c(name = name, version = "1.0")
  info$exports <- new.env(hash = TRUE, parent = baseenv())
  info$imports <- list(base = TRUE)
  info$path <- tempdir()
  info$dynlibs <- character()
  info$S3methods <- matrix(NA_character_, 0L, 4L)
  assign(".packageName", name, envir = ns)
  invisible(.Internal(registerNamespace(name, ns)))
  ns
}
inherited <- function(f) {
  env <- environment(getGeneric(f))
  all <- env$.AllMTable
  direct <- names(env$.MTable)
  sort(setdiff(if (is.null(all)) character() else names(all), direct))
}
direct <- function(f) sort(names(environment(getGeneric(f))$.MTable))

old <- make_ns("oldonly")
setOldClass(c("as_bytes", "numeric"), numeric(), where = old)
setOldClass("tagged_thing", where = old)
print(get(".requireCachedGenerics", envir = old))
b <- structure(c(1, 2), class = c("as_bytes", "numeric"))
print(unclass(b + 1))
print(is(b, "numeric"))
for (round in 1:2) {
  cacheMetaData(old, TRUE, old)
  print(existsMethod("+", c("as_bytes", "numeric")))
  print(isVirtualClass("as_bytes"))
  print(extends("as_bytes", "numeric"))
  print(getClass("tagged_thing")@className)
  print(unclass(b * 2))
}

gen <- make_ns("ownmethods")
setClass("Point", slots = c(x = "numeric", y = "numeric"), where = gen)
setClass("Point3", contains = "Point", slots = c(z = "numeric"), where = gen)
setGeneric("norm2", function(p, ...) standardGeneric("norm2"), where = gen)
setMethod("norm2", "Point", function(p, ...) sqrt(p@x^2 + p@y^2), where = gen)
setMethod("show", "Point", function(object) cat("<Point", object@x, object@y, ">\n"), where = gen)
setMethod("length", "Point", function(x) 2L, where = gen)
for (round in 1:2) {
  cacheMetaData(gen, TRUE, gen)
  norm2 <- get("norm2", envir = gen)
  p3 <- new("Point3", x = 3, y = 4, z = 0)
  print(norm2(p3))
  print(length(p3))
  show(p3)
  print(direct("norm2"))
  print(inherited("norm2"))
}

grp <- make_ns("groupmethods")
setClass("Money", slots = c(amount = "numeric"), where = grp)
setClass("Euro", contains = "Money", where = grp)
setMethod("Arith", c("Money", "Money"),
          function(e1, e2) new(class(e1), amount = callGeneric(e1@amount, e2@amount)), where = grp)
setMethod("Compare", c("Money", "numeric"),
          function(e1, e2) callGeneric(e1@amount, e2), where = grp)
setMethod("Ops", c("Money", "character"), function(e1, e2) "ops-fallback", where = grp)
setMethod("Math", "Money", function(x) new(class(x), amount = callGeneric(x@amount)), where = grp)
setOldClass(c("as_bytes2", "numeric"), numeric(), where = grp)
for (round in 1:2) {
  cacheMetaData(grp, TRUE, grp)
  e <- new("Euro", amount = 5)
  print((e + e)@amount)
  print(class(e - new("Money", amount = 1)))
  print(e > 3)
  print(e == "x")
  print(abs(new("Money", amount = -2))@amount)
  print(direct("+"))
  print(inherited("+"))
  print(direct("Arith"))
  print(direct(">"))
  print(inherited(">"))
}

# Detaching the metadata again (attach = FALSE) and caching it once more.
cacheMetaData(grp, FALSE, grp)
cacheMetaData(grp, TRUE, grp)
print((new("Euro", amount = 2) * new("Euro", amount = 4))@amount)
print(direct("*"))

# Caching a namespace that asks for Ops resets the inherited methods every member of the group
# has cached, so dispatch selects them again.
e <- new("Euro", amount = 1)
invisible(e + e); invisible(e > 0)
print(inherited("+"))
print(inherited(">"))
cacheMetaData(old, TRUE, old)
print(inherited("+"))
print(inherited(">"))
print((e + e)@amount)
print(inherited("+"))
