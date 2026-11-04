# Class, generic and method definitions made inside a package namespace, the shape the native
# definitions in ry's methods core take.
info <- new.env(hash = TRUE, parent = baseenv())
imports <- new.env(hash = TRUE, parent = .BaseNamespaceEnv)
attr(imports, "name") <- "imports:s4core"
ns <- new.env(hash = TRUE, parent = imports)
assign(".__NAMESPACE__.", info, envir = ns)
info$spec <- c(name = "s4core", version = "1.0")
info$exports <- new.env(hash = TRUE, parent = baseenv())
info$imports <- list(base = TRUE)
info$path <- tempdir()
info$dynlibs <- character()
info$S3methods <- matrix(NA_character_, 0L, 4L)
assign(".packageName", "s4core", envir = ns)
invisible(.Internal(registerNamespace("s4core", ns)))

setClass("V", representation = "VIRTUAL", where = ns)
setClass("W", representation = "VIRTUAL", where = ns)
genA <- setClass("A", slots = c(a = "numeric", f = "character", z = "logical"),
  prototype = list(a = 0), where = ns)
setClass("B", contains = "A", slots = c(b = "integer", o = "A"), prototype = list(b = 2L),
  where = ns)
setClass("C", contains = "B", where = ns)
setClass("D", slots = c(d = "character"), contains = "V", where = ns)
setClass("E", slots = c(e = "integer"), where = ns)
setClass("F", slots = c(g = "list"), contains = c("E", "V"), where = ns)
setClass("G", contains = c("D", "W"), slots = c(h = "numeric"),
  prototype = list(h = 1, d = "dee"), where = ns)
setClass("H", contains = "G", where = ns)
setClass("I", slots = c(i = "E", j = "V"), where = ns)

cat(class(genA), genA@className, "\n")
genA(a = 3, f = "x")
new("B")
new("C", a = 5, b = 7L)@b
new("H")@d
new("I")@i
cat(names(getClass("C", where = ns)@contains), "\n")
vapply(getClass("H", where = ns)@contains, function(e) e@distance, 1)
lapply(getClass("H", where = ns)@contains, function(e) e@by)
cat(names(getClass("A", where = ns)@subclasses), "\n")
cat(names(getClass("V", where = ns)@subclasses), "\n")
cat(names(getClass("D", where = ns)@subclasses), "\n")
is(new("C"), "A"); is(new("H"), "V"); is(new("H"), "W"); extends("F", "V"); extends("E", "F")
isVirtualClass("V", where = ns); isVirtualClass("G", where = ns)
getClass("G", where = ns)

c1 <- new("C", a = 1, f = "c", b = 3L)
as(c1, "A")
as(c1, "B")@b
as(c1, "A") <- new("A", a = 9, f = "replaced")
c1@a; c1@f; c1@b
h <- new("H", h = 4)
as(h, "D")
as(h, "D") <- new("D", d = "new d")
h@d
tryCatch(as(h, "V") <- new("D"), error = function(e) conditionMessage(e))

setGeneric("area", function(shape, ...) standardGeneric("area"), where = ns)
setMethod("area", "A", function(shape, ...) shape@a * 2, where = ns)
setMethod("area", "D", function(shape, ...) nchar(shape@d), where = ns)
area <- get("area", envir = ns)
area(new("C", a = 21))
area(new("H"))
tryCatch(area(new("E")), error = function(e) grepl("unable to find an inherited method", conditionMessage(e)))
existsMethod("area", "A", where = ns); hasMethod("area", "C", where = ns)

tryCatch(setClass("Bad", contains = "Nope", where = ns), error = function(e) conditionMessage(e))
tryCatch(setClass("Bad2", slots = c(x = "NoSuchClass"), where = ns),
  warning = function(w) conditionMessage(w))
tryCatch(setClass("Bad3", slots = c(a = "numeric"), contains = "A", where = ns),
  error = function(e) conditionMessage(e))
tryCatch(setClass("Bad4", slots = c(x = "numeric"), prototype = list(y = 1), where = ns),
  error = function(e) conditionMessage(e))
tryCatch(new("V"), error = function(e) conditionMessage(e))

# A class reached through two direct subclasses: adding a direct subclass rebuilds the
# superclass's subclass list, which then names that class twice.
setClass("Mx", contains = "VIRTUAL", slots = c(Dim = "integer"), validity = function(object) TRUE,
  where = ns)
setClass("lMx", contains = c("VIRTUAL", "Mx"), slots = c(x = "logical"), where = ns)
setClass("spMx", contains = c("VIRTUAL", "Mx"), where = ns)
setClass("CspMx", contains = c("VIRTUAL", "spMx"), slots = c(i = "integer"),
  validity = function(object) TRUE, where = ns)
setClass("lspMx", contains = c("VIRTUAL", "lMx", "spMx"), where = ns)
setClass("lgCMx", contains = c("CspMx", "lspMx"), validity = function(object) TRUE, where = ns)
setClass("indMx", contains = "spMx", slots = c(perm = "integer"), prototype = list(perm = 1L),
  validity = function(object) TRUE, where = ns)
setClass("pMx", contains = "indMx", validity = function(object) TRUE, where = ns)
for (k in c("Mx", "spMx", "CspMx", "indMx")) print(names(getClass(k, where = ns)@subclasses))
