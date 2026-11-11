# oracle: requires rlang
# A method that runs in slots hands its formals to `NextMethod()` as promises over its own frame.
# Each way the next method can keep such a promise past both returns must still read the value
# the method assigned: through native code (rlang's C captures the promise and its environment),
# a condition or restart that carries the next method's environment up to a handler above or
# below, `delayedAssign` into the global environment, a closure made in a helper, `sys.frames`,
# a finalizer registered on the frame, `environment()` returned or stored, `on.exit`, and
# `substitute` or `match.call` evaluated after both returns. The method or the next method also
# captures `sys.frames()`, `sys.function()` and `parent.frame()`.
stash <- new.env()

native <- function(x, y) UseMethod("native")
native.a <- function(x, y) rlang::enquo(y)
native.b <- function(x, y) { y <- paste("native", y); NextMethod() }

signalled <- function(x, y) UseMethod("signalled")
signalled.a <- function(x, y) {
  signalCondition(structure(class = c("carry", "condition"),
    list(message = "carry", call = NULL, env = environment())))
  "signalled.a"
}
signalled.b <- function(x, y) { y <- paste("signalled", y); NextMethod() }
below <- function(x, y) UseMethod("below")
below.a <- signalled.a
below.b <- function(x, y) {
  y <- paste("below", y)
  withCallingHandlers(NextMethod(), carry = function(condition) stash$below <- condition$env)
}

restarted <- function(x, y) UseMethod("restarted")
restarted.a <- function(x, y) invokeRestart("keep", environment())
restarted.b <- function(x, y) {
  y <- paste("restarted", y)
  withRestarts(NextMethod(), keep = function(env) env)
}

delayed <- function(x, y) UseMethod("delayed")
delayed.a <- function(x, y) { delayedAssign("late_y", y, assign.env = globalenv()); "delayed.a" }
delayed.b <- function(x, y) { y <- paste("delayed", y); NextMethod() }

made <- function(x, y) UseMethod("made")
made.a <- function(x, y) { make <- function() function() y; make() }
made.b <- function(x, y) { y <- paste("made", y); NextMethod() }

frames <- function(x, y) UseMethod("frames")
frames.a <- function(x, y) Filter(function(env) exists("y", envir = env, inherits = FALSE), sys.frames())
frames.b <- function(x, y) { y <- paste("frames", y); NextMethod() }

finalized <- function(x, y) UseMethod("finalized")
finalized.a <- function(x, y) {
  reg.finalizer(environment(), function(env) stash$finalized <- get("y", envir = env))
  "finalized.a"
}
finalized.b <- function(x, y) { y <- paste("finalized", y); NextMethod() }

object <- function(i) structure(i, class = c("b", "a"))
for (i in 1:40) {
  quosure <- native(object(i), i)
  withCallingHandlers(signalled(object(i), i), carry = function(condition) stash$above <- condition$env)
  below(object(i), i)
  restarted_env <- restarted(object(i), i)
  delayed(object(i), i)
  closure <- made(object(i), i)
  kept_frames <- frames(object(i), i)
  finalized(object(i), i)
  gc()
  r <- c(
    rlang::eval_tidy(quosure),
    rlang::quo_text(quosure),
    get("y", envir = stash$above),
    get("y", envir = stash$below),
    get("y", envir = restarted_env),
    late_y,
    closure(),
    vapply(kept_frames, function(env) format(get("y", envir = env)), ""),
    stash$finalized
  )
  if (i == 1 || i == 40) print(r)
}

captured <- function(x, y) UseMethod("captured")
captured.a <- function(x, y) {
  stash$next_function <- sys.function()
  stash$next_parent <- parent.frame()
  stash$next_frames <- sys.frames()
  "captured.a"
}
captured.b <- function(x, y) {
  y <- paste("captured", y)
  stash$method_function <- sys.function()
  stash$method_parent <- parent.frame()
  r <- NextMethod()
  stash$method_frames <- sys.frames()
  stash$method_env <- environment()
  r
}
returned <- function(x, y) UseMethod("returned")
returned.a <- function(x, y) environment()
returned.b <- function(x, y) { y <- paste("returned", y); NextMethod() }
exits <- function(x, y) UseMethod("exits")
exits.a <- function(x, y) { on.exit(stash$exit_next <- y); "exits.a" }
exits.b <- function(x, y) {
  y <- paste("exits", y)
  on.exit(stash$exit_method <- y, add = TRUE)
  NextMethod()
}
quoted <- function(x, y) UseMethod("quoted")
quoted.a <- function(x, y) list(substitute(y), match.call(), environment())
quoted.b <- function(x, y) { y <- paste("quoted", y); NextMethod() }
own_y <- function(frames) {
  frames <- Filter(function(env) exists("y", envir = env, inherits = FALSE), frames)
  vapply(frames, function(env) format(get("y", envir = env)), "")
}
caller <- function(i) { marker <- i; captured(object(i), i) }
for (i in 1:40) {
  caller(i)
  returned_env <- returned(object(i), i)
  exits(object(i), i)
  q <- quoted(object(i), i)
  r <- c(
    identical(stash$method_function, captured.b),
    identical(stash$next_function, captured.a),
    get("marker", envir = stash$method_parent) == i,
    get("marker", envir = stash$next_parent) == i,
    own_y(stash$method_frames),
    own_y(stash$next_frames),
    get("y", envir = stash$method_env),
    get("y", envir = returned_env),
    stash$exit_next,
    stash$exit_method,
    deparse(q[[1]]),
    deparse(q[[2]]),
    eval(q[[1]], q[[3]]),
    eval(q[[2]][[3]], parent.frame())
  )
  if (i == 1 || i == 40) print(r)
}
