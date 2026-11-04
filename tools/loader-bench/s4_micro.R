# Per-call cost of S4 definitions, as GNU R and ry run them: `Rscript s4_micro.R` against
# `ry run s4_micro.R`. Each case defines `n` fresh classes or methods in the global
# environment and prints the mean time per call in milliseconds.
#
# The global environment is not a namespace, so ry runs most definitions here through the
# methods package's R code; a package load takes the native path. `ry load` with
# `RY_LOAD_VERBOSE=true` reports every definition that falls back to the R code.

suppressPackageStartupMessages(library(methods))
n <- as.integer(Sys.getenv("S4_MICRO_N", "200"))
counter <- 0L
fresh <- function(prefix) {
  counter <<- counter + 1L
  paste0(prefix, counter)
}
per_call <- function(label, define) {
  gc()
  start <- proc.time()[["elapsed"]]
  for (i in seq_len(n)) define()
  elapsed <- proc.time()[["elapsed"]] - start
  cat(sprintf("%-38s %8.3f ms\n", label, elapsed * 1000 / n))
}

setClass("Root0", slots = c(r = "numeric"))
chain <- function(depth) {
  parent <- "Root0"
  for (level in seq_len(depth - 1L)) {
    child <- fresh("Chain")
    setClass(child, contains = parent)
    parent <- child
  }
  parent
}
parents <- lapply(c(1L, 3L, 10L), chain)
setClassUnion("AnyRoot", c("Root0", "NULL"))
setGeneric("describe", function(x, ...) standardGeneric("describe"))
setGeneric("describe2", function(x, y, ...) standardGeneric("describe2"))

per_call("setClass, no parent", function() setClass(fresh("Plain"), slots = c(a = "numeric")))
per_call("setClass, parent depth 1", function() setClass(fresh("D1_"), contains = parents[[1]]))
per_call("setClass, parent depth 3", function() setClass(fresh("D3_"), contains = parents[[2]]))
per_call("setClass, parent depth 10", function() setClass(fresh("D10_"), contains = parents[[3]]))
per_call("setClass, contains numeric", function() setClass(fresh("Num"), contains = "numeric"))
per_call("setClass, contains list", function() setClass(fresh("Lst"), contains = "list", slots = c(m = "character")))
per_call("setClass + setIs into a union", function() {
  name <- fresh("U")
  setClass(name, slots = c(a = "numeric"))
  setIs(name, "AnyRoot")
})

classes <- vapply(seq_len(n), function(i) {
  name <- fresh("M")
  setClass(name, contains = "Root0")
  name
}, "")
method <- function(make) {
  i <- 0L
  function() {
    i <<- i + 1L
    make(classes[[i]])
  }
}
per_call("setMethod, same formals", method(function(cl) setMethod("describe", cl, function(x, ...) 1)))
per_call("setMethod, other formals (.local)", method(function(cl) setMethod("describe", cl, function(x, verbose = FALSE, ...) 2)))
per_call("setMethod, two-argument signature", method(function(cl) setMethod("describe2", c(cl, "numeric"), function(x, y, ...) 3)))
per_call("setMethod, base generic (as.list)", method(function(cl) setMethod("as.list", cl, function(x, ...) list())))
