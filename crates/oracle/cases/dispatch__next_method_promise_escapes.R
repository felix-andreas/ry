# oracle: requires rlang
# A method that runs in slots hands its formals to `NextMethod()` as promises over its own frame.
# Each way the next method can keep such a promise past both returns must still read the value
# the method assigned: through native code (rlang's C captures the promise and its environment),
# a condition or restart that carries the next method's environment up to a handler above or
# below, `delayedAssign` into the global environment, a closure made in a helper, `sys.frames`,
# and a finalizer registered on the frame.
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
