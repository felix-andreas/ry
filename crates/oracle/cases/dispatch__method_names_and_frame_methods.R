# A method bound in the calling frame after the frame already dispatched is found by the next
# dispatch from that frame, however the binding appears: `<-`, `assign`, or `eval` in the frame.
# Each function runs many times so its body compiles and the dispatch takes the compiled paths.
describe <- function(x) UseMethod("describe")
describe.default <- function(x) "default"
thing <- structure(1, class = c("thing", "base"))

by_arrow <- function(x) {
  before <- describe(x)
  describe.thing <- function(x) "arrow"
  c(before, describe(x))
}
by_assign <- function(x) {
  before <- describe(x)
  assign("describe.base", function(x) "assign", envir = environment())
  c(before, describe(x))
}
by_eval <- function(x) {
  before <- describe(x)
  eval(quote(describe.thing <- function(x) "eval"), environment())
  c(before, describe(x))
}
for (i in 1:30) r <- list(by_arrow(thing), by_assign(thing), by_eval(thing))
str(r)

# The same for an internal generic, whose dispatch can run before the frame has an environment.
size <- function(x) {
  before <- length(x)
  length.thing <- function(x) 99L
  c(before, length(x))
}
for (i in 1:30) r <- size(thing)
print(r)

# One method name made by two splits: `measure.data.frame` is the `measure` method for class
# "data.frame" and the `measure.data` method for class "frame".
measure <- function(x) UseMethod("measure")
measure.data <- function(x) UseMethod("measure.data")
measure.data.frame <- function(x) "measure.data.frame"
measure.default <- function(x) "measure.default"
measure.data.default <- function(x) "measure.data.default"
df <- data.frame(a = 1)
framed <- structure(1, class = "frame")
for (i in 1:30) r <- c(measure(df), measure.data(framed), measure(framed), measure.data(df))
print(r)

# Many classes on one generic: each finds its own method or the default.
classes <- paste0("k", 1:40)
for (k in classes[c(TRUE, FALSE)]) {
  assign(paste0("describe.", k), local({ name <- k; function(x) name }))
}
for (i in 1:3) r <- vapply(classes, function(k) describe(structure(1, class = k)), "")
print(unname(r))
