# A warning reaches its handlers natively when no calling handler takes it first; every case here
# must look exactly as if `.signalSimpleWarning` had run.
show <- function(label, expr) {
  cat("--", label, "\n")
  str(withVisible(tryCatch(expr, error = function(e) {
    cat("error call:", deparse(conditionCall(e)), "\n")
    paste("error:", conditionMessage(e))
  })))
}
coerce <- function(x) as.integer(x)
raise <- function() warning("raised")

show("tryCatch warning, C level", tryCatch(coerce("a"), warning = function(w) list(class(w), conditionMessage(w), conditionCall(w))))
show("tryCatch warning, R level", tryCatch(raise(), warning = function(w) list(class(w), conditionMessage(w), conditionCall(w))))
show("tryCatch condition", tryCatch(coerce("b"), condition = function(c) class(c)))
show("tryCatch simpleWarning", tryCatch(coerce("c"), simpleWarning = function(w) "simple"))
show("tryCatch other class", tryCatch(tryCatch(coerce("d"), message = function(m) "message"), warning = function(w) "outer"))
show("tryCatch unwinds", {
  f <- function() {
    on.exit(cat("on.exit ran\n"))
    coerce("e")
    cat("not reached\n")
  }
  tryCatch(f(), warning = function(w) "caught", finally = cat("finally ran\n"))
})
show("tryCatch frames", tryCatch(coerce("f"), warning = function(w) length(sys.calls())))
show("tryCatch value is the handler's", tryCatch({ coerce("g"); "body" }, warning = function(w) invisible("handler")))
show("calling then exiting", tryCatch(withCallingHandlers(coerce("h"), warning = function(w) cat("calling saw it\n")), warning = function(w) "exited"))
show("exiting then calling", withCallingHandlers(tryCatch(coerce("i"), warning = function(w) "exited"), warning = function(w) cat("never\n")))
show("calling, non-matching class", withCallingHandlers(coerce("j"), message = function(m) cat("never\n"), error = function(e) cat("never\n")))
show("calling, matching", withCallingHandlers(coerce("k"), warning = function(w) {
  cat("calling:", conditionMessage(w), deparse(conditionCall(w)), length(sys.calls()) > 0, "\n")
  invokeRestart("muffleWarning")
}))
show("calling, condition class", withCallingHandlers(raise(), condition = function(c) {
  cat("condition:", class(c)[1], "\n")
  invokeRestart("muffleWarning")
}))
show("restart visible to calling handler", withCallingHandlers(coerce("l"), warning = function(w) {
  cat("muffleWarning available:", !is.null(computeRestarts(w)[[1]]), "\n")
  invokeRestart("muffleWarning")
}))
show("nested tryCatch picks the inner", tryCatch(tryCatch(coerce("m"), warning = function(w) "inner"), warning = function(w) "outer"))
show("in a loop", {
  hits <- 0
  for (i in 1:50) hits <- hits + tryCatch(coerce("n"), warning = function(w) 1)
  hits
})

op <- options(warn = 2)
show("warn 2, C level", coerce("o"))
show("warn 2, R level", raise())
show("warn 2, tryCatch", tryCatch(coerce("p"), warning = function(w) "caught first"))
show("warn 2, calling error handler", withCallingHandlers(coerce("q"), error = function(e) cat("error handler:", conditionMessage(e), "\n")))
options(op)
op <- options(warn = 1)
show("warn 1", coerce("r"))
options(op)
op <- options(warn = -1)
show("warn -1", coerce("s"))
options(op)
show("warn 0", coerce("t"))
cat("last.warning:", names(get0("last.warning", envir = baseenv())), "\n")
tryCatch(coerce("u"), warning = function(w) NULL)
cat("last.warning after tryCatch:", names(get0("last.warning", envir = baseenv())), "\n")
print(warnings())
