# `%in%` and unlist() answer natively once their frames have forced the arguments, when the
# arguments are calls rather than values. Results, attributes, forcing order, errors, warnings
# and what the argument code sees of the stack match the R bodies.
show <- function(value) { print(value); invisible(value) }
attempt <- function(expr) tryCatch(show(expr), error = function(e) {
  cat("error:", conditionMessage(e), "\n")
  print(conditionCall(e))
})
s <- "abc"; v <- c(1, 2, 3)
for (i in 1:3) {
  show(paste(s) %in% paste(c("abc", "d")))
  show(paste(s, 1:3) %in% paste("abc", 2))
  show(identity(2) %in% identity(v))
  show(unlist(lapply(list(c(a = 1), c(b = 2, c = 3)), identity)))
  show(unlist(lapply(list(c(a = "p"), "q"), names), use.names = FALSE))
}

# `%in%` of every kind of value.
show(identity(NA) %in% identity(c(1, NA)))
show(identity(NULL) %in% identity(1:3))
show(identity(1:3) %in% identity(NULL))
show(identity(factor(c("a", "b"))) %in% identity("b"))
show(identity(as.Date("2024-01-02")) %in% identity(as.Date(c("2024-01-02", "2024-01-03"))))
show(identity(list(1, "a")) %in% identity(list("a")))
show(identity(c(a = 1, b = 2)) %in% identity(2))
show(identity(matrix(1:4, 2)) %in% identity(3:9))
show(identity(1i) %in% identity(c(1i, 2)))
attempt(identity(quote(a)) %in% identity("a"))
attempt(identity(sum) %in% identity(1))
tagged <- structure(1:3, class = "tagged")
mtfrm.tagged <- function(x) { cat("mtfrm method\n"); as.character(unclass(x) * 10) }
show(identity(tagged) %in% identity("20"))
show(`%in%`(table = identity(1:2), x = identity(2)))

# `%in%` forces `x`, then `table`, in its own frame.
order <- character()
note <- function(label, value) { order <<- c(order, label); value }
show(note("x", 1) %in% note("table", 1:2))
show(order)
inside <- function() { print(vapply(sys.calls(), function(call) deparse(call)[[1]], "")); "a" }
show(inside() %in% "a")
show("a" %in% inside())
attempt(stop("in x") %in% 1)
attempt(1 %in% stop("in table"))
show(withCallingHandlers(as.integer("x") %in% NA,
  warning = function(w) { print(conditionCall(w)); invokeRestart("muffleWarning") }))
g <- function(a) missing(a) %in% TRUE
show(g())
attempt(`%in%`(1))

# unlist() of lists, nested lists, pairlists, expressions and plain values.
show(unlist(identity(list(1, 2L, "a"))))
show(unlist(identity(list(a = 1, b = list(c = 2, d = list(e = 3))))))
show(unlist(identity(list(a = 1, b = list(c = 2))), recursive = FALSE))
show(unlist(identity(list(a = 1:2, b = 3)), use.names = FALSE))
show(unlist(identity(list())))
show(unlist(identity(NULL)))
show(unlist(identity(1:3)))
show(unlist(identity(c(a = 1))))
show(unlist(identity(pairlist(a = 1, b = "x"))))
show(unlist(identity(expression(a, 1))))
show(unlist(identity(list(sum, 1))))
show(is.environment(unlist(identity(new.env()))))
show(unlist(identity(list(a = NULL, b = 1))))
show(unlist(identity(list(list(), list(1)))))
show(unlist(identity(list(factor("a"), factor(c("b", "a"))))))
show(unlist(identity(list(x = factor("a"), y = list(factor("b"))))))
show(unlist(identity(list(factor("a"), 1))))
show(unlist(identity(list(as.Date("2024-01-02"), 1))))
show(unlist(identity(structure(list(a = 1), extra = "dropped"))))

# unlist() dispatches internally on objects.
classy <- structure(list(1, 2), class = "classy")
unlist.classy <- function(x, recursive = TRUE, use.names = TRUE) "unlist method"
show(unlist(identity(classy)))
rm(unlist.classy)
show(unlist(identity(classy)))
show(unlist(identity(data.frame(a = 1:2, b = c("x", "y")))))

# The flags: checked before `x` is forced, forced in the R body's order, and passed through.
order <- character()
show(unlist(note("x", list(a = 1)), note("recursive", TRUE), note("use.names", FALSE)))
show(order)
attempt(unlist(list(1), recursive = NA))
attempt(unlist(list(1), recursive = c(TRUE, FALSE)))
attempt(unlist(list(1), recursive = sum))
show(unlist(list(a = 1), recursive = 1))
show(unlist(list(a = 1), use.names = 0))
attempt(unlist(list(a = 1), use.names = NA))
attempt(unlist(list(a = 1), use.names = "yes"))
show(unlist(list(a = 1), use.names = "TRUE"))
attempt(unlist(list(a = 1), use.names = c(TRUE, FALSE)))
attempt(unlist(list(a = 1), use.names = logical(0)))
attempt(unlist(list(a = 1), use.names = NULL))
attempt(unlist(list(a = 1), use.names = list(TRUE)))
show(unlist(expression(a, 1), recursive = FALSE))
show(unlist(list(1, expression(b))))
show(c(expression(a), 1, recursive = TRUE))
show(c(expression(a), 1))
show(unlist(list(a = list(b = 1)), recursive = structure(FALSE, note = "kept")))
show(unlist(list(1), TRUE, FALSE))
attempt(unlist())

# unlist() forces `x` in its own frame.
show(unlist(list(inside())))
attempt(unlist(stop("in x")))
show(withVisible(unlist(invisible(list(1)))))
h <- function(a) unlist(list(missing(a)))
show(h())

# A user's `%in%` and unlist() mask base's.
local({
  `%in%` <- function(x, table) "mine"
  unlist <- function(x, recursive = TRUE, use.names = TRUE) "mine too"
  show(paste(s) %in% paste(v))
  show(unlist(lapply(1:2, identity)))
})
