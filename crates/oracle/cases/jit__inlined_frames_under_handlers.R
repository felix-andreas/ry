# Compiled code runs a closure call without a frame while a calling handler is established. A
# condition signalled inside such a call reaches the handler under the frames the interpreter
# would have built: the handler sees the callee's call on the stack, can read and assign the
# caller's variables, and an argument nobody forced before the condition is unforced when the
# handler runs. Each callee runs on its own first, so that a caller compiled early knows it.

checked <- function(x, limit) {
  if (x > limit) warning("over the limit: ", x)
  x + 1
}
for (i in 1:30) checked(i, 100)
total <- function(a, b, limit) checked(a, limit) + checked(b, limit)
for (i in 1:30) total(i, i, 100)

stacks <- list()
messages <- character()
watch <- function(code) {
  withCallingHandlers(code, warning = function(w) {
    calls <- sys.calls()
    named <- vapply(calls, function(call) deparse(call[[1L]])[1L], "")
    stacks[[length(stacks) + 1L]] <<- named[named %in% c("total", "checked", "warning")]
    messages <<- c(messages, paste(conditionMessage(w), "from", deparse(conditionCall(w))))
    invokeRestart("muffleWarning")
  })
}
for (round in 1:40) watch(total(round, 2, 100))
print(length(stacks))
print(watch(total(1, 2, 100)))
print(watch(total(200, 2, 100)))
print(watch(total(3, 300, 100)))
print(watch(total(400, 500, 100)))
print(stacks)
print(messages)
for (round in 1:40) watch(total(round, 2, 100))
print(watch(total(600, 2, 100)))
print(messages)

# An error inside a frameless call runs the calling handler before the stack unwinds.
strict <- function(x) {
  if (!is.numeric(x)) stop("not a number: ", class(x))
  x * 2
}
for (i in 1:30) strict(i)
doubled <- function(a, b) strict(a) + strict(b)
for (i in 1:30) doubled(i, i)
report <- function(value) {
  tryCatch(
    withCallingHandlers(doubled(1, value), error = function(e) {
      cat("handler sees", sys.nframe() > 0L, deparse(sys.call(-1L))[1L], "\n")
      cat("failing call", deparse(conditionCall(e)), "\n")
    }),
    error = function(e) conditionMessage(e)
  )
}
for (round in 1:40) report(round)
print(report(2))
print(report("text"))
print(report(NULL))
print(report(3))

# A handler assigns a variable of the caller, and the rest of the caller reads the new value.
scaled <- function(x) {
  if (x < 0) warning("negative")
  x * 10
}
for (i in 1:30) scaled(i)
adjust <- function(x) {
  offset <- 1
  first <- scaled(x)
  first + offset
}
for (i in 1:30) adjust(i)
handled_adjust <- function(x) {
  withCallingHandlers(adjust(x), warning = function(w) {
    frame <- sys.frame(which(vapply(sys.calls(), function(call) {
      identical(call[[1L]], quote(adjust))
    }, NA))[1L])
    assign("offset", 1000, envir = frame)
    invokeRestart("muffleWarning")
  })
}
for (round in 1:40) handled_adjust(round)
print(handled_adjust(4))
print(handled_adjust(-4))
print(handled_adjust(5))

# An argument the callee has not read when the condition is signalled is unforced when the
# handler runs, and it is forced once, afterwards.
pick <- function(flag, value) {
  if (flag) warning("flagged")
  value
}
for (i in 1:30) pick(FALSE, i)
forced <- 0L
lazy <- function(flag) pick(flag, { forced <<- forced + 1L; forced })
for (i in 1:30) lazy(FALSE)
forced <- 0L
order <- character()
watch_lazy <- function(flag) {
  withCallingHandlers(lazy(flag), warning = function(w) {
    order <<- c(order, paste("handler with", forced, "forced"))
    invokeRestart("muffleWarning")
  })
}
for (round in 1:40) watch_lazy(FALSE)
print(forced)
print(watch_lazy(TRUE))
print(watch_lazy(FALSE))
print(watch_lazy(TRUE))
print(order)

# `message` and a condition of a custom class under handlers that return without a restart.
note <- function(x) {
  if (x %% 7L == 0L) message("seven divides ", x)
  if (x %% 11L == 0L) signalCondition(structure(class = c("custom", "condition"),
                                                list(message = paste("eleven", x), call = NULL)))
  x
}
for (i in 1:6) note(i)
sum_notes <- function(n) {
  s <- 0L
  for (i in seq_len(n)) s <- s + note(i)
  s
}
for (round in 1:40) sum_notes(6L)
log <- character()
print(withCallingHandlers(
  sum_notes(30L),
  message = function(m) {
    log <<- c(log, trimws(conditionMessage(m)))
    invokeRestart("muffleMessage")
  },
  custom = function(c) log <<- c(log, conditionMessage(c))
))
print(log)

# An error with a class of its own is signalled where it is raised, so a calling handler must
# find the inlined callee's frame and the caller's variables.
element <- function(x, i) x[[i]]
column <- function(x, i) x[, i]
show <- function(e) {
  cat(class(e)[1], conditionMessage(e), "\n")
  calls <- sys.calls()
  print(calls[seq(length(calls) - 2L, length(calls) - 1L)])
  print(get("n", envir = sys.frame(-2)))
}
probe <- function(callee, good, bad) {
  caller <- eval(substitute(
    function(r, k) { n <- length(r); v <- CALLEE(r, k); list(n, v) },
    list(CALLEE = as.name(callee))
  ))
  environment(caller) <- globalenv()
  for (i in 1:40) do.call(callee, good)
  for (i in 1:40) do.call(caller, good)
  for (class in c("error", "condition", "subscriptOutOfBoundsError", "simpleError")) {
    handlers <- structure(list(show), names = class)
    tryCatch(
      do.call(withCallingHandlers, c(list(quote(caller(bad[[1]], bad[[2]]))), handlers)),
      error = function(e) cat("caught", class(e)[1], "\n")
    )
  }
}
probe("element", list(list(1, 2, 3), 3L), list(list(1), 3L))
probe("element", list(c(a = 1, b = 2), "b"), list(c(a = 1), "b"))
probe("element", list(c("a", "b", "c"), 3L), list("a", 3L))
probe("column", list(matrix(1:6, 3), 2L), list(matrix(1:6, 3), 9L))
