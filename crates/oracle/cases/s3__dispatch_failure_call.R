# A dispatch that finds no method is an error of the `UseMethod` call, not of the generic's.
show <- function(expr) {
  e <- tryCatch(expr, error = function(e) e)
  cat(class(e)[1], "|", conditionMessage(e), "|", deparse(conditionCall(e)), "\n")
}
g <- function(x) UseMethod("g")
show(g(1))
show(g(1))
show(g(structure(1, class = c("a", "b"))))
h <- function(x, ...) {
  UseMethod("h")
}
show(h("a"))
on_second <- function(x, y) UseMethod("on_second", y)
show(on_second(1, TRUE))
show(on_second(1, TRUE))
computed <- function(x) UseMethod(paste0("comp", "uted"))
show(computed(NULL))
named <- function(x) UseMethod(generic = "named")
show(named(sum))
show(conditionMessage(1))
# A failure inside the method names the method's call.
g.default <- function(x) stop("in the method")
show(g(1))
r <- try(h(1L), silent = TRUE)
cat(r)
