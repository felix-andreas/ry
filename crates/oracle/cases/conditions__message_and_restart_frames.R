# message() and withRestarts() as calling handlers observe them: the frames on the stack, the
# variables each frame holds, the restart objects, handler order, muffling, on.exit, and the
# text message() builds from its arguments.
frame_names <- function() {
    calls <- sys.calls()
    vapply(calls, function(call) if (is.name(call[[1L]])) as.character(call[[1L]]) else "<fn>", "")
}
show_frames <- function(names) print(names[seq_len(max(0L, length(names) - 1L))])
withCallingHandlers(
    message("hello ", 1L, " ", 2.5, " ", TRUE),
    message = function(c) {
        cat("message:", conditionMessage(c))
        print(conditionCall(c))
        print(class(c))
        show_frames(frame_names())
        restarts <- computeRestarts(c)
        print(vapply(restarts, function(r) r[[1L]], ""))
        restart <- restarts[[1L]]
        print(names(unclass(restart)))
        print(class(restart))
        print(restart$description)
        print(restart$interactive)
        print(restart$test(c))
        print(is.function(restart$handler))
        print(is.environment(restart$exit))
        invokeRestart("muffleMessage")
    }
)
# The variables of the withRestarts() and message() frames, read from inside a handler.
withCallingHandlers(
    message("x"),
    message = function(c) {
        frames <- sys.frames()
        names <- frame_names()
        for (name in c("message", "withRestarts", "withOneRestart", "doWithOneRestart")) {
            position <- match(name, names)
            cat(name, ":", sort(ls(frames[[position]], all.names = TRUE)), "\n")
        }
        position <- match("message", names)
        print(get("cond", frames[[position]]))
        print(get("msg", frames[[position]]))
        print(get("call", frames[[position]]))
        position <- match("withRestarts", names)
        restarts <- get("restarts", frames[[position]])
        print(length(restarts))
        print(names(restarts))
        print(restarts[[1L]]$name)
        invokeRestart("muffleMessage")
    }
)
# Handler order: the inner handler runs first, a muffle stops the outer one, and a handler that
# declines lets the next one run.
order <- character()
withCallingHandlers(
    withCallingHandlers(
        { message("one"); message("two"); order <- c(order, "body") },
        message = function(c) {
            order <<- c(order, paste("inner", conditionMessage(c)))
            if (conditionMessage(c) == "one\n") invokeRestart("muffleMessage")
        }
    ),
    message = function(c) {
        order <<- c(order, paste("outer", conditionMessage(c)))
        invokeRestart("muffleMessage")
    }
)
print(order)
# An exiting handler, a muffle through suppressMessages(), and a message signalled from a handler.
print(tryCatch({ message("caught"); "not reached" }, message = function(c) conditionMessage(c)))
f <- function() {
    on.exit(cat("on.exit f\n"))
    suppressMessages({ message("quiet"); "after" })
}
print(f())
g <- function() {
    on.exit(cat("on.exit g\n"))
    withCallingHandlers(
        message("outer"),
        message = function(c) {
            if (conditionMessage(c) == "outer\n") {
                message("from handler")
                invokeRestart("muffleMessage")
            }
        }
    )
    "g done"
}
print(g())
# The text message() builds: no arguments, appendLF, NA, numbers, factors, whitespace, a condition
# object, and a condition with extra arguments, which warns.
capture <- function(expr) {
    text <- NULL
    withCallingHandlers(expr, message = function(c) {
        text <<- conditionMessage(c)
        invokeRestart("muffleMessage")
    })
    text
}
print(capture(message()))
print(capture(message("a", "b", appendLF = FALSE)))
print(capture(message(NA, NA_character_, NA_real_, NULL, character())))
print(capture(message(1/3, 1e-20, 100000, 1e5 + 0.1, -0, 1L:3L)))
print(capture(message(factor("level"), as.Date("2024-01-02"))))
print(capture(message("  padded  ", "\t")))
print(capture(message("été ", "日本")))
print(capture(message(c("a", "b"), list("x", 2))))
print(capture(message("domain NA", domain = NA)))
condition <- simpleCondition("custom text\n", call = quote(h(x)))
class(condition) <- c("myMessage", "message", "condition")
print(capture(message(condition)))
print(withCallingHandlers(
    capture(message(condition, "ignored")),
    warning = function(w) {
        cat("warning:", conditionMessage(w), "\n")
        invokeRestart("muffleWarning")
    }
))
print(withVisible(suppressMessages(message("v")))$visible)
# A message nobody handles goes to stderr.
message("to stderr ", 42)
message("no newline", appendLF = FALSE)
message("")
# withRestarts() with each kind of restart specification.
print(withRestarts(invokeRestart("fn", 1, 2), fn = function(a, b) a + b))
print(withRestarts(
    vapply(computeRestarts()[1L], function(r) r$description, ""),
    described = "a description"
))
print(withRestarts(invokeRestart("listed", 5), listed = list(handler = function(x) x * 2,
    description = "from a list")))
print(withRestarts(
    vapply(computeRestarts(), function(r) r[[1L]], "")[1:3],
    first = function() 1, second = function() 2, third = "third"
))
print(withRestarts({ cat("plain body\n"); "value" }, unused = function() NULL))
print(withVisible(withRestarts(invisible(7), r = function() NULL))$visible)
print(tryCatch(withRestarts(1, function() NULL), error = function(e) conditionMessage(e)))
print(withRestarts(
    computeRestarts()[[1L]]$test(NULL),
    tested = list(handler = function() NULL, test = function(c) FALSE)
))
print(withRestarts(
    invokeRestart(computeRestarts()[[1L]], "by object"),
    obj = function(x) x
))
h <- function() {
    on.exit(cat("on.exit h\n"))
    withRestarts({
        on.exit(cat("on.exit added from the restart body\n"), add = TRUE)
        invokeRestart("leave", "left")
    }, leave = function(x) x)
}
print(h())
k <- function() {
    withRestarts(
        withRestarts(invokeRestart("outer", "skipped inner"), inner = function() "inner"),
        outer = function(x) x
    )
}
print(k())
z <- 0
spec <- function() { z <<- z + 1; NULL }
print(withRestarts(invokeRestart("counted"), counted = spec))
print(z)
