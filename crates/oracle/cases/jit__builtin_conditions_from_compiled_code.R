# A compiled body calls a builtin without first asking whether a handler is established. A
# builtin that warns or fails there signals the condition as the interpreter does: a calling
# handler runs under the body's frame, sees the locals assigned before the call and none
# assigned after it, may change them, and an exiting handler or a restart leaves from the same
# place. Each body runs on its own first, so it is compiled before a handler surrounds it.

scaled <- function(values, text) {
  factor <- length(values) * 2L
  shift <- as.integer(text)
  total <- factor + shift
  total
}
for (i in 1:60) scaled(1:3, "4")

seen <- list()
watch <- function(code) {
  withCallingHandlers(code, warning = function(w) {
    calls <- sys.calls()
    named <- vapply(calls, function(call) deparse(call[[1L]])[1L], "")
    frame <- sys.frame(which(named == "scaled")[1L])
    seen[[length(seen) + 1L]] <<- list(
      message = conditionMessage(w),
      call = deparse(conditionCall(w)),
      locals = sort(ls(frame)),
      factor = get("factor", envir = frame)
    )
    invokeRestart("muffleWarning")
  })
}
for (round in 1:40) watch(scaled(1:3, "4"))
print(length(seen))
print(watch(scaled(1:3, "four")))
print(watch(scaled(1:5, "4")))
print(watch(scaled(c(1.5, 2.5), "x")))
str(seen)

# The handler changes a local, and the body goes on with the new value.
adjust <- function(code) {
  withCallingHandlers(code, warning = function(w) {
    calls <- sys.calls()
    named <- vapply(calls, function(call) deparse(call[[1L]])[1L], "")
    assign("factor", 100L, envir = sys.frame(which(named == "scaled")[1L]))
    invokeRestart("muffleWarning")
  })
}
for (round in 1:40) adjust(scaled(1:3, "4"))
print(adjust(scaled(1:3, "4")))
print(adjust(scaled(1:3, "none")))
print(scaled(1:3, "5"))

# Exiting handlers, a muffled warning, and a warning turned into an error.
print(tryCatch(scaled(1:3, "x"), warning = function(w) conditionMessage(w)))
print(tryCatch(scaled(1:3, "x"), warning = function(w) deparse(conditionCall(w))))
print(suppressWarnings(scaled(1:3, "x")))
print(tryCatch(scaled(1:3, "6"), warning = function(w) "unreached"))
old <- options(warn = 2)
print(tryCatch(scaled(1:3, "x"), error = function(e) conditionMessage(e)))
options(old)
order_seen <- character()
print(tryCatch(
  withCallingHandlers(scaled(1:3, "x"), warning = function(w) {
    order_seen <<- c(order_seen, "calling")
  }),
  warning = function(w) {
    order_seen <<- c(order_seen, "exiting")
    "caught"
  }
))
print(order_seen)
for (round in 1:60) suppressWarnings(scaled(1:3, "x"))
print(suppressWarnings(scaled(1:3, "x")))
print(scaled(1:3, "7"))

# A builtin that fails: the error names the builtin's call, a calling handler runs before the
# stack unwinds, and `finally` and `on.exit` code run once.
pick <- function(items, index) {
  size <- length(items)
  on.exit(exits <<- exits + 1L)
  chosen <- items[[index]]
  chosen * size
}
exits <- 0L
for (i in 1:60) pick(list(1, 2, 3), 2L)
print(exits)
describe <- function(code) {
  tryCatch(code, error = function(e) paste(conditionMessage(e), "in", deparse(conditionCall(e))))
}
for (round in 1:40) describe(pick(list(1, 2, 3), 1L))
print(describe(pick(list(1, 2, 3), 3L)))
print(describe(pick(list(1, 2, 3), 7L)))
print(describe(pick(list(1, "a", 3), 2L)))
print(describe(pick(NULL, 1L)))
exits <- 0L
reached <- character()
print(tryCatch(
  withCallingHandlers(pick(list(1, 2), 9L), error = function(e) {
    calls <- sys.calls()
    named <- vapply(calls, function(call) deparse(call[[1L]])[1L], "")
    frame <- sys.frame(which(named == "pick")[1L])
    reached <<- c(reached, sort(ls(frame)), as.character(get("size", envir = frame)))
  }),
  error = function(e) "unwound",
  finally = reached <- c(reached, "finally")
))
print(reached)
print(exits)
print(inherits(try(pick(list(1, 2), 9L), silent = TRUE), "try-error"))
for (round in 1:60) describe(pick(list(1, 2, 3), 7L))
print(describe(pick(list(1, 2, 3), 7L)))
print(pick(list(1, 2, 3), 3L))

# Arithmetic and mathematical builtins on vectors warn and fail the same way.
ratio <- function(left, right) {
  sum <- left + right
  root <- sqrt(sum)
  max(root)
}
for (i in 1:60) ratio(c(1, 2, 3), c(3, 2, 1))
collect <- function(code) {
  messages <- character()
  value <- withCallingHandlers(code, warning = function(w) {
    messages <<- c(messages, paste(conditionMessage(w), "in", deparse(conditionCall(w))))
    invokeRestart("muffleWarning")
  })
  list(value = value, messages = messages)
}
for (round in 1:40) collect(ratio(c(1, 2, 3), c(3, 2, 1)))
str(collect(ratio(c(1, 2, 3), c(3, 2, 1))))
str(collect(ratio(c(1, 2, 3), c(1, 2))))
str(collect(ratio(c(-5, 2, 3), c(1, 2, 3))))
str(collect(ratio(c(-5, 2, 3), c(1, 2))))
str(collect(ratio(numeric(0), numeric(0))))
print(describe(ratio(c(1, 2), "a")))
print(describe(ratio(list(1), 2)))
str(collect(ratio(c(1, 2, 3), c(3, 2, 1))))
