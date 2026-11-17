# Coercing a list to an atomic type converts each element on its own, so every element that
# becomes NA, leaves the integer range or loses an imaginary part raises its own warning; raw
# warns about out-of-range values once for the whole list. An empty or list element becomes NA,
# and an element longer than one, a function or NULL is an error.
show <- function(expr) {
  result <- withCallingHandlers(
    tryCatch(expr, error = function(e) paste("error:", conditionMessage(e))),
    warning = function(w) { cat("  warning:", conditionMessage(w), "\n"); invokeRestart("muffleWarning") })
  print(result)
}
show(as.integer(list("a", "b", 2, "c")))
show(as.integer(list(1e10, 2, -1e10)))
show(as.integer(list(1+1i, 2+2i, 3)))
show(as.integer(list("1", "x", 3e10, 2i)))
show(as.double(list("a", 1, "b")))
show(as.double(list(1i, 2i)))
show(as.complex(list("a", "b", 1)))
show(as.logical(list("a", "b", 2, 2i)))
show(as.raw(list(1, 300, "a", -1, "b")))
show(as.raw(list(1, 2)))
show(as.integer(list(integer(0), 1)))
show(as.double(list(list(1), 2)))
show(as.complex(list(character(0), list("z"))))
show(as.integer(list(NULL, 2)))
show(as.integer(list(1:2, 2)))
show(as.integer(list(sum, 2)))
show(as.double(list(character(0), "z")))
show(as.integer(pairlist("a", "b", 3)))
show(as.integer(as.pairlist(list("a", 1e10))))
x <- list("a", "b", "7"); show({ storage.mode(x) <- "integer"; x })
x <- list("a", "b"); show({ mode(x) <- "numeric"; x })
show(as.vector(list("q", "r"), "integer"))
x <- matrix(list(1L, "a", 2, "b", "c", 3i), 2); show({ storage.mode(x) <- "integer"; x })
count <- 0
withCallingHandlers(as.integer(as.list(letters)),
  warning = function(w) { count <<- count + 1; invokeRestart("muffleWarning") })
print(count)
