# A union whose `new()` dispatches `initialize` to a package method runs that method, which the
# native union must not skip. A union with an undefined member leaves the dispatch caches alone.
info <- new.env(hash = TRUE, parent = baseenv())
imports <- new.env(hash = TRUE, parent = .BaseNamespaceEnv)
attr(imports, "name") <- "imports:s4unioninit"
ns <- new.env(hash = TRUE, parent = imports)
assign(".__NAMESPACE__.", info, envir = ns)
info$spec <- c(name = "s4unioninit", version = "1.0")
info$exports <- new.env(hash = TRUE, parent = baseenv())
info$imports <- list(base = TRUE)
info$path <- tempdir()
info$dynlibs <- character()
info$S3methods <- matrix(NA_character_, 0L, 4L)
assign(".packageName", "s4unioninit", envir = ns)
invisible(.Internal(registerNamespace("s4unioninit", ns)))

entry <- function() {
  table <- methods:::.getMethodsTable(getGeneric("initialize"), inherited = TRUE)
  if (!"ClassUnionRepresentation" %in% names(table)) return("none")
  class(table[["ClassUnionRepresentation"]])
}
setClass("A", representation(x = "numeric"), where = ns)
setClass("B", contains = "A", where = ns)
try(setClassUnion("Broken", c("A", "Nope"), where = ns))
entry()
setMethod("initialize", "classRepresentation", function(.Object, ...) {
  cat("package initialize for", class(.Object), "\n")
  callNextMethod()
}, where = ns)
setClass("C", contains = "A", where = ns)
setClassUnion("U1", c("numeric", "character"), where = ns)
entry()
setClassUnion("U2", c("A", "NULL"), where = ns)
names(getClass("U2", where = ns)@subclasses)
# Once the dispatch caches hold the package methods, a plain class and a class that extends
# another still call them.
setClass("D", representation(y = "numeric"), where = ns)
setMethod("initialize", "SClassExtension", function(.Object, ...) {
  cat("package initialize for", class(.Object), "\n")
  callNextMethod()
}, where = ns)
setClass("E", contains = "A", where = ns)
setClass("F", contains = "A", where = ns)
names(getClass("F", where = ns)@contains)
