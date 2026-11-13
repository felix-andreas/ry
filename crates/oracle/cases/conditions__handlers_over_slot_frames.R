# Condition handling from compiled closures, which run on slots until something needs their
# environment. Each function runs many times so its body compiles.

# An error return: the frame's environment is not carried by the unwind, but a closure or a
# condition that holds it keeps it alive.
checked <- function(x, name = "x") {
  if (!is.numeric(x)) stop(sprintf("`%s` must be numeric", name), call. = FALSE)
  x
}
caught <- function(n) {
  tryCatch(checked("a", paste0("v", n)), error = function(e) conditionMessage(e))
}
for (i in 1:30) r <- caught(i)
print(r)

keeps_env <- function(n) {
  local_value <- n
  condition <- simpleCondition("kept")
  class(condition) <- c("kept", "error", "condition")
  condition$env <- environment()
  stop(condition)
}
for (i in 1:30) r <- tryCatch(keeps_env(i), kept = function(e) get("local_value", envir = e$env))
print(r)

keeps_closure <- function(n) {
  getter <- function() n * 2
  condition <- simpleCondition("closure")
  class(condition) <- c("closure_condition", "error", "condition")
  condition$getter <- getter
  stop(condition)
}
for (i in 1:30) r <- tryCatch(keeps_closure(i), closure_condition = function(e) e$getter())
print(r)

# `..1` in `stop` and in user code, from a frame running on slots.
first_dot <- function(...) {
  if (...length() == 1L && is.character(..1)) paste("one:", ..1) else ...length()
}
for (i in 1:30) r <- list(first_dot("a"), first_dot(1, 2), first_dot())
str(r)
missing_dot <- function(...) tryCatch(..2, error = function(e) conditionMessage(e))
for (i in 1:30) r <- missing_dot(1)
print(r)

# tryCatch with finally, nested tryCatch and the handler's view of the frames.
with_finally <- function(n) {
  log <- character()
  value <- tryCatch(
    {
      log <- c(log, "body")
      if (n %% 2 == 0) stop("even") else n
    },
    error = function(e) {
      log <<- c(log, "handler")
      -1
    },
    finally = log <- c(log, "finally")
  )
  list(value, log)
}
for (i in 1:30) r <- list(with_finally(i), with_finally(i + 1))
str(r)

nested <- function(n) {
  tryCatch(
    {
      tryCatch(stop("inner ", n), warning = function(w) "wrong handler")
    },
    error = function(e) paste("outer caught:", conditionMessage(e))
  )
}
for (i in 1:30) r <- nested(i)
print(r)

rethrown <- function(n) {
  tryCatch(
    tryCatch(stop("first"), error = function(e) stop(paste("second after", conditionMessage(e)))),
    error = function(e) conditionMessage(e)
  )
}
for (i in 1:30) r <- rethrown(i)
print(r)

# withCallingHandlers with invokeRestart, and a warning that continues.
muffled <- function(n) {
  seen <- character()
  value <- withCallingHandlers(
    {
      warning("w", n)
      message("m", n)
      n * 2
    },
    warning = function(w) {
      seen <<- c(seen, conditionMessage(w))
      invokeRestart("muffleWarning")
    },
    message = function(m) {
      seen <<- c(seen, trimws(conditionMessage(m)))
      invokeRestart("muffleMessage")
    }
  )
  list(value, seen)
}
for (i in 1:30) r <- muffled(i)
str(r)

custom_restart <- function(n) {
  withRestarts(
    withCallingHandlers(
      {
        signalCondition(structure(class = c("ask", "condition"), list(message = "ask", call = NULL)))
        "not restarted"
      },
      ask = function(c) invokeRestart("use_value", n * 10)
    ),
    use_value = function(value) value
  )
}
for (i in 1:30) r <- custom_restart(i)
print(r)

# conditionCall names the call that signalled, and sys.calls() inside a handler sees the
# frames still on the stack.
failing <- function(x) stop("failed on ", x)
calls_of <- function(n) {
  call <- tryCatch(failing(n), error = function(e) conditionCall(e))
  depth <- withCallingHandlers(
    failing_quietly(n),
    warning = function(w) {
      calls <- sys.calls()
      heads <- vapply(calls, function(call) deparse(call[[1L]])[1L], "")
      handler_heads <<- heads[heads %in% c("calls_of", "failing_quietly", "warning")]
      invokeRestart("muffleWarning")
    }
  )
  list(call, depth)
}
failing_quietly <- function(x) {
  warning("careful")
  x + 1
}
handler_heads <- NULL
for (i in 1:30) r <- calls_of(i)
str(r)
print(handler_heads)

# A closure made by a frame on slots looks up free names through the frame: names the frame
# binds, names it leaves to its enclosure, a name it assigns later, and `<<-` into it.
outer_frame <- function(n) {
  bound <- n
  inner <- function() {
    environment()
    counter <<- counter + 1L
    c(bound, length(list(1, 2)), later, counter)
  }
  counter <- 0L
  later <- n * 3
  first <- inner()
  second <- inner()
  c(first, second, counter)
}
for (i in 1:30) r <- outer_frame(i)
print(r)

# A method's inner closure sees the dispatch variables of the method frame.
describe <- function(x) UseMethod("describe")
describe.thing <- function(x) {
  show <- function() {
    environment()
    paste(.Generic, class(x)[1L])
  }
  show()
}
for (i in 1:30) r <- describe(structure(i, class = "thing"))
print(r)
