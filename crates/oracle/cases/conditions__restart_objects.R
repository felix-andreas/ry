# The restart objects withRestarts() builds, as suppressWarnings() and suppressMessages()
# establish them: their fields, names, class and the frames they close over.
seen <- NULL
withCallingHandlers(
  suppressWarnings({
    warning("inner")
    "value"
  }),
  warning = function(w) seen <<- w
)
is.null(seen)

# The top-level restart differs between front ends, so only restarts made by withRestarts()
# are listed.
restart_names <- function(r) {
  vapply(Filter(function(x) !is.null(names(x)), r), function(x) x$name, "")
}

describe <- function(r) {
  list(
    class = class(r),
    names = names(r),
    name = r$name,
    exit_is_env = is.environment(r$exit),
    handler = deparse(r$handler),
    description = r$description,
    test_args = names(formals(r$test)),
    test_body = deparse(body(r$test)),
    test_frame = sort(ls(environment(r$test))),
    test_value = r$test(simpleCondition("x")),
    interactive = r$interactive
  )
}

# The muffle restart a warning handler sees while suppressWarnings() is active.
suppressWarnings(withCallingHandlers(
  warning("w"),
  warning = function(w) {
    r <- computeRestarts(w)
    print(restart_names(r))
    str(describe(r[[1]]))
  }
))

# A restart given as a function and one given as a description.
withRestarts(
  {
    r <- computeRestarts()
    str(describe(r[[1]]))
    str(describe(r[[2]]))
    print(r[[1]])
    print(r[[2]])
    NULL
  },
  first = function(x) x + 1,
  second = "described"
)

# The frame makeRestart() leaves holds its arguments, and the default handler closes over it.
withRestarts(
  {
    r <- computeRestarts()[[1]]
    frame <- environment(r$handler)
    print(sort(ls(frame)))
    print(identical(environment(r$test), frame))
    print(get("description", frame))
    print(get("name", frame))
    print(r$handler())
    NULL
  },
  only = "only described"
)

# Changing one restart's names or class leaves every other restart untouched.
changed <- withRestarts(
  {
    r <- computeRestarts()[[1]]
    names(r)[1] <- "label"
    class(r) <- c("mine", class(r))
    r
  },
  muffle = function() NULL
)
names(changed)
class(changed)
withRestarts(
  {
    r <- computeRestarts()[[1]]
    print(names(r))
    print(class(r))
    NULL
  },
  muffle = function() NULL
)

# invokeRestart("muffleWarning") from a calling handler continues after warning().
f <- function() {
  warning("first")
  warning("second")
  "done"
}
caught <- character()
withCallingHandlers(f(), warning = function(w) {
  caught <<- c(caught, conditionMessage(w))
  invokeRestart("muffleWarning")
})
caught

# conditionCall() of a muffled warning names the call that warned, through every frame.
g <- function(x) {
  warning("from g")
  x
}
withCallingHandlers(g(1), warning = function(w) {
  print(conditionCall(w))
  invokeRestart("muffleWarning")
})
suppressWarnings(withCallingHandlers(g(2), warning = function(w) print(conditionCall(w))))

# Nested handlers: the innermost suppressWarnings() muffles first, and an outer handler
# sees only what escapes it.
outer <- character()
withCallingHandlers(
  {
    suppressWarnings(warning("hidden"))
    suppressMessages({
      message("hidden too")
      warning("shown")
    })
    "end"
  },
  warning = function(w) {
    outer <<- c(outer, conditionMessage(w))
    invokeRestart("muffleWarning")
  }
)
outer
withCallingHandlers(
  suppressWarnings(
    withCallingHandlers(
      warning("twice"),
      warning = function(w) cat("inner handler:", conditionMessage(w), "\n")
    )
  ),
  warning = function(w) cat("outer handler:", conditionMessage(w), "\n")
)

# The restarts in reach from inside nested muffles, innermost first.
suppressMessages(suppressWarnings(withCallingHandlers(
  warning("w"),
  warning = function(w) print(restart_names(computeRestarts()))
)))
suppressWarnings(suppressMessages(withCallingHandlers(
  message("m"),
  message = function(m) print(restart_names(computeRestarts(m)))
)))

# The frames withRestarts() and withOneRestart() leave hold their inner functions.
inspect_frames <- function() {
    calls <- sys.calls()
    frames <- sys.frames()
    at <- which(vapply(calls, function(call) identical(call[[1]], quote(withRestarts)), NA))
    print(sort(ls(frames[[at]])))
    print(sort(ls(frames[[at + 1]])))
    print(sort(ls(frames[[at + 2]])))
    print(vapply(calls[at + 0:2], function(call) deparse(call[[1]]), ""))
    print(identical(sys.function(at), withRestarts))
    print(identical(environment(frames[[at]]$makeRestart), frames[[at]]))
    print(names(frames[[at]]$restarts[[1]]))
    print(identical(frames[[at + 2]]$restart$exit, frames[[at + 2]]))
    NULL
}
withRestarts(inspect_frames(), done = function() NULL)
suppressWarnings(withCallingHandlers(warning("x"), warning = function(w) inspect_frames()))

# A restart invoked with arguments runs its handler through docall().
withRestarts(invokeRestart("again", 2, 3), again = function(a, b) a * b)
withRestarts(
  invokeRestart(computeRestarts()[[1]], "object"),
  by_object = function(x) paste("invoked by", x)
)
