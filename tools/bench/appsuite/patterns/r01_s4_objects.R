# bench(label, f) times f(i). It doubles the number of calls until one batch takes 0.3 s of CPU
# time, then prints the label and the nanoseconds per call of the last batch. With the environment
# variable UNDER_HANDLER=1, the calls run inside withCallingHandlers(), as the code of a test does.
under_handler <- nzchar(Sys.getenv("UNDER_HANDLER"))
run <- function(n, f) for (i in seq_len(n)) f(i)
bench <- function(label, f) {
  n <- 64
  repeat {
    start <- sum(proc.time()[1:2])
    if (under_handler) withCallingHandlers(run(n, f), warning = function(w) NULL) else run(n, f)
    seconds <- sum(proc.time()[1:2]) - start
    if (seconds >= 0.3) break
    n <- n * 2
  }
  cat(sprintf("%s\t%.0f\n", label, 1e9 * seconds / n))
}

# Pattern: small S4 objects. Code creates them, reads and writes slots through generics, asks
# for their class, and calls methods that take more arguments than their generic.
library(methods)

setClass("Base", representation("VIRTUAL", id = "character"))
setClass("Item", contains = "Base", representation(value = "numeric", day = "Date", meta = "list"),
  validity = function(object) {
    if (length(object@id) != 1) return("id must have length 1")
    if (length(object@value) != 1) return("value must have length 1")
    TRUE
  }
)
setClass("SubItem", contains = "Item", representation(factor = "numeric"))
setMethod("initialize", "Item", function(.Object, ...) {
  .Object <- callNextMethod(.Object, ...)
  if (length(.Object@value) == 0) .Object@value <- 0
  .Object
})

setGeneric("getValue", function(object, ...) standardGeneric("getValue"))
setMethod("getValue", "Item", function(object, key = NA_character_, scale = 1, ...) {
  if (!is.na(key)) return(object@meta[[key]])
  object@value * scale
})
setMethod("getValue", "SubItem", function(object, key = NA_character_, scale = 1, ...) {
  callNextMethod() * object@factor
})
setGeneric("id", function(object) standardGeneric("id"))
setMethod("id", "Base", function(object) object@id)
setGeneric("value<-", function(object, value) standardGeneric("value<-"))
setReplaceMethod("value", "Item", function(object, value) {
  object@value <- value
  object
})

day <- as.Date("2024-01-31")
item <- new("Item", id = "a", value = 1, day = day, meta = list(k = 2))
sub <- new("SubItem", id = "b", value = 1, day = day, meta = list(k = 2), factor = 3)
items <- lapply(1:20, function(i) new("Item", id = paste0("k", i), value = i, day = day))

bench("s4: new() with initialize and validity", function(i) new("Item", id = "a", value = i, day = day))
bench("s4: generic with one argument", function(i) id(item))
bench("s4: method with extra arguments (.local)", function(i) getValue(item, scale = 2))
bench("s4: callNextMethod()", function(i) getValue(sub))
bench("s4: slot read x@value", function(i) item@value)
bench("s4: replacement generic value(x) <- v", function(i) { value(item) <- i; item })
bench("s4: slot write x@value <- v in a function", function(i) { x <- item; x@value <- i; x })
bench("s4: is(x, 'Base')", function(i) is(item, "Base"))
bench("s4: slotNames(x)", function(i) slotNames(item))
bench("s4: slot(x, name)", function(i) slot(item, "value"))
bench("s4: validObject(x)", function(i) validObject(item))
bench("s4: isVirtualClass('Base')", function(i) isVirtualClass("Base"))
bench("s4: sapply over 20 objects, one generic each", function(i) sapply(items, id))
bench("s4: lapply over the slots of one object", function(i) lapply(slotNames(item), function(s) slot(item, s)))
