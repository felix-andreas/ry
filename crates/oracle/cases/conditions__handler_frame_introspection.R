# The frames of tryCatch(), withCallingHandlers(), suppressWarnings(), suppressMessages() and try()
# as code running inside them sees them: from the expression, an exiting handler, a calling
# handler and finally, at one and two levels of nesting, through sys.calls(), sys.function(),
# ls(sys.frame()), parent.frame(), sys.parents() and the promises the helper frames hold.
show_frames <- function(label) {
  n <- sys.nframe() - 1L
  cat("--", label, "\n")
  for (i in seq_len(n)) {
    cat(i, ":", paste(deparse(sys.call(i), width.cutoff = 500L), collapse = " "), "\n")
    cat("   formals:", paste(names(formals(sys.function(i))), collapse = ","), "\n")
    cat("   locals:", paste(ls(sys.frame(i), all.names = TRUE), collapse = ","), "\n")
  }
  cat("   parents:", paste(sys.parents()[seq_len(n)], collapse = " "), "\n")
  cat("   caller is global:", identical(parent.frame(), globalenv()), "\n")
}

cat("== one level\n")
tryCatch(show_frames("expr"), error = function(e) NULL)
tryCatch(stop("boom"), error = function(e) show_frames("exiting handler"))
tryCatch(NULL, finally = show_frames("finally"))
withCallingHandlers(show_frames("calling expr"), warning = function(w) NULL)
withCallingHandlers(
  warning("w"),
  warning = function(w) {
    show_frames("calling handler")
    invokeRestart("muffleWarning")
  }
)
suppressWarnings(show_frames("suppressWarnings expr"))
suppressMessages(show_frames("suppressMessages expr"))
invisible(try(show_frames("try expr")))

cat("== two levels\n")
tryCatch(
  tryCatch(show_frames("inner expr"), warning = function(w) NULL),
  error = function(e) NULL,
  message = function(m) NULL
)
tryCatch(
  tryCatch(stop("inner"), warning = function(w) NULL),
  error = function(e) show_frames("outer handler"),
  finally = show_frames("outer finally")
)
tryCatch(
  withCallingHandlers(
    message("hello"),
    message = function(m) show_frames("calling under exiting")
  ),
  message = function(m) show_frames("exiting after calling")
)
f <- function(x) tryCatch(g(x), error = function(e) show_frames("handler in a closure"))
g <- function(x) if (x) stop("no") else show_frames("expr in a closure")
f(TRUE)
f(FALSE)

cat("== the helper frames' bindings\n")
peek <- function() {
  frames <- sys.frames()
  for (i in seq_along(frames)[-length(frames)]) {
    env <- frames[[i]]
    for (name in intersect(c("expr", "name", "names", "nh", "handler"), ls(env))) {
      if (name == "expr") {
        cat(i, "expr code:", deparse(eval(call("substitute", as.name("expr")), env)), "\n")
      } else {
        cat(i, name, ":", deparse(get(name, envir = env)), "\n")
      }
    }
  }
  "peeked"
}
tryCatch(peek(), warning = function(w) 1, error = function(e) 2)
tryCatch(peek(), error = function(e) 2)
tryCatch(peek())

cat("== visibility and jumps\n")
tryCatch(invisible(1))
tryCatch(invisible(2), error = function(e) 0)
tryCatch(stop("x"), error = function(e) invisible(3))
print(withVisible(tryCatch(4, error = function(e) 0))$visible)
early <- function() {
  tryCatch(return("returned"), error = function(e) "error")
  "fell through"
}
early()
loop <- function() {
  seen <- integer()
  for (i in 1:5) {
    tryCatch({
      if (i == 2L) next
      if (i == 4L) break
      seen <- c(seen, i)
    }, error = function(e) NULL, finally = cat("finally", i, "\n"))
  }
  seen
}
loop()
order <- function() {
  on.exit(cat("on.exit\n"))
  tryCatch({
    on.exit(cat("never: tryCatch's expr has no frame of its own\n"), add = TRUE)
    stop("e")
  }, error = function(e) cat("handler\n"), finally = cat("finally\n"))
}
order()
nested_return <- function() {
  tryCatch(tryCatch(return("deep"), warning = function(w) "w"), error = function(e) "e")
  "not reached"
}
nested_return()

cat("== caught conditions\n")
cond <- tryCatch(stop("plain"), error = identity)
print(conditionCall(cond))
cond <- tryCatch(stop("no call", call. = FALSE), error = identity)
print(conditionCall(cond))
h <- function() stop("from h")
cond <- tryCatch(h(), error = identity)
print(conditionCall(cond))
cond <- tryCatch(log(-1), warning = identity)
print(conditionCall(cond))
cond <- tryCatch(sqrt("a"), error = identity)
print(conditionCall(cond))
custom <- structure(class = c("custom", "error", "condition"), list(message = "m", call = NULL))
print(tryCatch(stop(custom), custom = function(e) class(e)))
print(tryCatch(stop(custom), error = function(e) "error first", custom = function(e) "custom"))
print(tryCatch(signalCondition(custom), condition = function(c) "signalled"))
print(tryCatch(message("note"), condition = function(c) class(c)))
print(tryCatch(warning("again"), warning = function(w) tryCatch(stop("in handler"), error = conditionMessage)))
handler <- function(e) "a handler that is not a function expression"
print(tryCatch(stop("x"), error = handler))
print(tryCatch(1 + 1, error = handler, finally = cat("finally after value\n")))
res <- try(stop("tried"), silent = TRUE)
cat(res)
print(class(res))
print(inherits(attr(res, "condition"), "simpleError"))
old <- options(warn = 2)
print(tryCatch(warning("now an error"), error = function(e) conditionMessage(e)))
options(old)
