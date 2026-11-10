# signalCondition() as handlers observe it: the variables of its frame, the conditionMessage(),
# conditionCall() and `$` methods it dispatches to, handler order, a handler that changes the
# condition's class, and conditions that are not condition lists.
frame_of <- function(name) {
    calls <- sys.calls()
    names <- vapply(calls, function(call) if (is.name(call[[1L]])) as.character(call[[1L]]) else "<fn>", "")
    sys.frames()[[match(name, names)]]
}
condition <- structure(class = c("custom", "condition"), list(message = "text", call = quote(f(1))))
withCallingHandlers(signalCondition(condition), custom = function(c) {
    frame <- frame_of("signalCondition")
    print(sort(ls(frame, all.names = TRUE)))
    print(get("msg", frame))
    print(get("call", frame))
    print(identical(get("cond", frame), c))
})
print(signalCondition(condition))
print(withVisible(signalCondition(condition))$visible)
# Methods of the user's own are called, in class order, before any handler runs.
conditionMessage.custom <- function(c) { cat("conditionMessage.custom\n"); "from method" }
conditionCall.custom <- function(c) { cat("conditionCall.custom\n"); quote(g()) }
withCallingHandlers(signalCondition(condition), condition = function(c) {
    frame <- frame_of("signalCondition")
    print(get("msg", frame))
    print(get("call", frame))
})
print(signalCondition(condition))
rm(conditionMessage.custom, conditionCall.custom)
`$.dollar` <- function(x, name) { cat("$.dollar", name, "\n"); paste("field", name) }
dollar <- structure(class = c("dollar", "condition"), list(message = "m", call = NULL))
withCallingHandlers(signalCondition(dollar), condition = function(c) {
    print(get("msg", frame_of("signalCondition")))
})
rm(`$.dollar`)
# A field found only by partial matching, a missing field, and extra fields.
partial <- structure(class = "condition", list(messageText = "partial", callee = quote(p())))
withCallingHandlers(signalCondition(partial), condition = function(c) {
    frame <- frame_of("signalCondition")
    print(get("msg", frame))
    print(get("call", frame))
})
empty <- structure(class = c("empty", "condition"), list())
withCallingHandlers(signalCondition(empty), condition = function(c) {
    print(is.null(get("msg", frame_of("signalCondition"))))
})
# A condition with no `call` field, as testthat's expectations are: `c$call` is NULL.
no_call <- structure(class = c("expectation_success", "expectation", "condition"), list(message = "ok", srcref = NULL))
withCallingHandlers(signalCondition(no_call), expectation = function(c) {
    frame <- frame_of("signalCondition")
    print(get("msg", frame))
    print(is.null(get("call", frame)))
})
# Handler order: the innermost first, an exiting handler ends the walk, and a handler that changes
# the class of the condition it holds does not change which handlers run.
order <- character()
result <- tryCatch(
    withCallingHandlers(
        withCallingHandlers(
            { signalCondition(condition); "body" },
            custom = function(c) order <<- c(order, "inner custom")
        ),
        condition = function(c) order <<- c(order, "outer condition")
    ),
    custom = function(c) { order <<- c(order, "exiting custom"); "caught" }
)
print(order)
print(result)
withCallingHandlers(
    withCallingHandlers(signalCondition(condition), custom = function(c) {
        class(c) <- "other"
        cat("inner sees", class(c), "\n")
    }),
    custom = function(c) cat("outer sees", class(c), "\n")
)
# Not a condition list: a string, NULL, and a classed atomic vector.
withCallingHandlers(signalCondition("plain text"), condition = function(c) {
    print(class(c))
    print(conditionMessage(c))
})
print(signalCondition(NULL))
atomic <- structure("atomic", class = c("atomicCondition", "condition"))
print(tryCatch(signalCondition(atomic), condition = function(c) class(c)))
# Unhandled warnings, with each value of the warn option.
f <- function() warning("from f")
old <- options(warn = 1)
f()
options(warn = 0)
g <- function() { f(); cat("after warning\n"); invisible() }
g()
options(warn = 2)
print(tryCatch(f(), error = function(e) conditionMessage(e)))
withCallingHandlers(
    tryCatch(f(), error = function(e) cat("error:", conditionMessage(e), "\n")),
    warning = function(w) cat("calling handler sees", conditionMessage(w), "\n")
)
options(old)
