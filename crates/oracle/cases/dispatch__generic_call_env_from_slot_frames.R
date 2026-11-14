# `.GenericCallEnv` is the frame a generic was called from. Few methods read it, so a caller that
# keeps its variables in slots makes its environment only when a method does. Each block reads the
# variable through a different route and checks it against `parent.frame()`.

describe <- function(x, ...) UseMethod("describe")

# Read by name in the method's body.
describe.default <- function(x, ...) {
  cat("default:", identical(.GenericCallEnv, parent.frame()), "\n")
  invisible(.GenericCallEnv)
}

# Not read here: `NextMethod` passes it on.
describe.inner <- function(x, ...) {
  cat("inner, then ")
  NextMethod()
}

# Read through the method's environment, one variable at a time.
describe.lookup <- function(x, ...) {
  found <- get(".GenericCallEnv")
  cat("get:", identical(found, parent.frame()), exists(".GenericCallEnv", inherits = FALSE), "\n")
  cat("eval:", identical(eval(quote(.GenericCallEnv)), parent.frame()), "\n")
  invisible(found)
}

# Read with every other variable of the method's frame.
describe.listed <- function(x, ...) {
  local_value <- x
  all <- as.list(environment(), all.names = TRUE)
  cat("names:", paste(sort(grep("^[.]Generic", names(all), value = TRUE)), collapse = " "), "\n")
  cat("listed:", identical(all$.GenericCallEnv, parent.frame()), "\n")
  invisible(all$.GenericCallEnv)
}

# Read by a closure after the method and the generic's caller have returned.
describe.kept <- function(x, ...) {
  function() .GenericCallEnv
}

from_function <- function(value) {
  local_variable <- 42
  env <- describe(value)
  cat("the caller's environment:", identical(env, environment()), "\n")
  cat("the caller's variables:", paste(sort(ls(env)), collapse = " "), "\n")
}
from_function(1)
from_function(structure(1, class = "inner"))
from_function(structure(1, class = "lookup"))
from_function(structure(1, class = "listed"))
from_function(structure(1, class = c("inner", "lookup")))
from_function(structure(1, class = c("inner", "listed")))

keep <- function(value) {
  marker <- "the caller of the generic"
  describe(value)
}
kept <- keep(structure(1, class = "kept"))
cat("kept:", get("marker", envir = kept()), "\n")
kept <- keep(structure(1, class = c("inner", "kept")))
cat("kept through NextMethod:", get("marker", envir = kept()), "\n")

# From top level the generic's caller is the global environment.
cat("top level:", environmentName(describe(structure(1, class = "lookup"))), "\n")
cat("top level:", environmentName(describe(structure(1, class = c("inner", "listed")))), "\n")

# `NextMethod` looks for the next method in the generic's caller first, so a method defined
# there is found from a method defined elsewhere.
with_local_methods <- function(value) {
  describe.outer <- function(x, ...) {
    cat("local outer, then ")
    NextMethod()
  }
  describe.last <- function(x, ...) {
    cat("local last:", identical(.GenericCallEnv, parent.frame()), "\n")
  }
  describe(value)
  invisible()
}
with_local_methods(structure(1, class = c("outer", "inner", "last")))

# A caller that binds no method of the generic, with the next method found in its enclosure.
make_caller <- function() {
  describe.enclosed <- function(x, ...) {
    cat("enclosed:", identical(.GenericCallEnv, parent.frame()), "\n")
  }
  function(value) {
    unrelated <- 1
    describe(value)
    invisible(unrelated)
  }
}
make_caller()(structure(1, class = c("inner", "enclosed")))

# The generic's caller is a loop body and a promise, not only a function body.
for (class in c("lookup", "listed")) {
  results <- lapply(1:2, function(i) describe(structure(i, class = c("inner", class))))
  cat("lapply:", length(results), "\n")
}
forced <- function(argument) argument
via_promise <- function(value) {
  local_variable <- 1
  env <- forced(describe(value))
  cat("promise:", identical(env, environment()), "\n")
}
via_promise(structure(1, class = c("inner", "lookup")))

# A group generic's method and an internal generic's method see their caller too.
Ops.money <- function(e1, e2) {
  cat("Ops:", .Generic, identical(.GenericCallEnv, parent.frame()), "\n")
  unclass(e1) + unclass(e2)
}
length.money <- function(x) {
  cat("length:", identical(get(".GenericCallEnv"), parent.frame()), "\n")
  99L
}
arithmetic <- function(amount) {
  local_variable <- 1
  total <- amount + amount
  c(total, length(amount))
}
print(arithmetic(structure(5, class = "money")))
