# warning() with plain arguments builds its message without .makeMessage(); everything a
# handler, a caller or the warning machinery can see must match the R body.
show <- function(w) cat(class(w)[[1L]], "|", conditionMessage(w), "|", deparse(conditionCall(w)), "\n")

f <- function(n) warning("n is ", n, ", ", NA, " and ", TRUE)
g <- function() warning("no call", call. = FALSE)
h <- function(...) warning(...)

# Handled: the condition, its call, and the frames the handler runs under.
withCallingHandlers(f(3L), warning = function(w) {
    show(w)
    calls <- vapply(sys.calls(), function(call) deparse(call)[[1L]], "")
    print(calls[grepl("warning|signal", calls)])
    invokeRestart("muffleWarning")
})
withCallingHandlers(g(), warning = function(w) { show(w); invokeRestart("muffleWarning") })
withCallingHandlers(h("passed ", 1.5, " through dots"), warning = function(w) {
    show(w)
    invokeRestart("muffleWarning")
})
show(tryCatch(f(2.25), warning = identity))
show(tryCatch(warning(c("a", "b"), 1:3, NULL), warning = identity))
show(tryCatch(warning(), warning = identity))

# The value is the message, and it is invisible.
value <- withVisible(suppressWarnings(f(1L)))
print(value)

# Unhandled, at each warn level.
old <- options(warn = 1)
f(4L)
g()
warning("immediate.", immediate. = TRUE)
options(warn = 2)
print(tryCatch(f(5L), error = conditionMessage))
options(warn = -1)
f(6L)
options(old)

# Arguments the R body handles: a condition, an object, a translation domain.
show(tryCatch(warning(simpleWarning("a condition", call = quote(here()))), warning = identity))
show(tryCatch(warning(factor("level")), warning = identity))
show(tryCatch(warning("no translation", domain = NA), warning = identity))
show(tryCatch(warning(structure("classed", class = "myclass")), warning = identity))
as.character.myclass <- function(x, ...) "from a method"
show(tryCatch(warning(structure("classed", class = "myclass")), warning = identity))

# A deferred warning, reported after the top-level call.
f(7L)
