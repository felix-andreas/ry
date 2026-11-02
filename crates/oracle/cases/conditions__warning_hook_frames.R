# A warning always runs through `.signalSimpleWarning(message, base::quote(call))`, even with no
# warning handler established, so a warning promoted to an error by `options(warn = 2)` shows
# that frame below the error handler. rlang classifies a signal by these frames.
f <- function() warning("boom")
heads <- function(calls) vapply(calls, function(call) deparse(call[[1]])[1], "")
h <- function(e) {
  calls <- sys.calls()
  print(heads(calls))
  print(calls[[grep("signalSimpleWarning", heads(calls))]])
  invokeRestart("out", NULL)
}
op <- options(warn = 2)
withRestarts(out = identity, withCallingHandlers(error = h, f()))
options(op)
g <- function() { warning("deferred"); "done" }
print(g())
