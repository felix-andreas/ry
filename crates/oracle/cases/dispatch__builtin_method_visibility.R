# A primitive's result is visible whether its own code or a method ran, so a method that
# returns through `invisible()` prints at top level and reports `visible = TRUE`. A closure
# generic keeps its method's invisibility.

length.quiet <- function(x) invisible(5L)
is.numeric.quiet <- function(x) invisible(FALSE)
"[.quiet" <- function(x, i) invisible(7L)
"[[.quiet" <- function(x, i) invisible(8L)
"$.quiet" <- function(x, name) invisible(name)
"+.quiet" <- function(e1, e2) invisible(9L)
"==.quiet" <- function(e1, e2) invisible(TRUE)
"!.quiet" <- function(x) invisible(NA)
as.character.quiet <- function(x, ...) invisible("a")
abs.quiet <- function(x) invisible(-1)
sum.quiet <- function(..., na.rm = FALSE) invisible(100)
rep.quiet <- function(x, ...) invisible("repeated")
dim.quiet <- function(x) invisible(c(2L, 2L))
names.quiet <- function(x) invisible("named")
xtfrm.quiet <- function(x) invisible(3:1)
c.quiet <- function(...) invisible("combined")
unlist.quiet <- function(x, ...) invisible("flat")
format.quiet <- function(x, ...) invisible("formatted")
x <- structure(1:3, class = "quiet")

length(x)
is.numeric(x)
x[1]
x[[1]]
x$field
x + 1
x == 1
!x
as.character(x)
abs(x)
sum(x)
rep(x, 2)
dim(x)
names(x)
xtfrm(x)
c(x, 1)
unlist(x)
format(x)
visible <- function(expr) withVisible(expr)$visible
print(c(
  visible(length(x)), visible(is.numeric(x)), visible(x[1]), visible(x[[1]]), visible(x$field),
  visible(x + 1), visible(x == 1), visible(!x), visible(as.character(x)), visible(abs(x)),
  visible(sum(x)), visible(rep(x, 2)), visible(dim(x)), visible(names(x)), visible(xtfrm(x)),
  visible(c(x, 1)), visible(unlist(x)), visible(format(x))
))
measure <- function(x) length(x)
numeric_like <- function(x) is.numeric(x)
print(c(visible(measure(x)), visible(numeric_like(x)), visible(measure(1:3))))
for (i in 1:60) measure(x)
print(visible(measure(x)))

# Replacement forms and assignments stay invisible, and `invisible()` on a primitive's value
# keeps its effect.
"names<-.quiet" <- function(x, value) invisible(x)
names(x) <- "a"
print(visible(names(x) <- "a"))
print(visible(invisible(length(x))))
hidden <- function(x) invisible(length(x))
hidden(x)
print(visible(hidden(x)))
