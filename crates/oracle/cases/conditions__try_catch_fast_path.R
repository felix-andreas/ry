# tryCatch() and suppressWarnings() as the stack, handlers and exit code see them: the frames
# sys.calls() and sys.function() show, the call a caught condition carries, handler order, finally
# and on.exit on every way out, and the muffleWarning restart.
calls_here <- function() {
  n <- sys.nframe() - 1L
  calls <- character(n)
  formals_seen <- character(n)
  for (i in seq_len(n)) {
    calls[[i]] <- paste(deparse(sys.call(i)), collapse = " ")
    formals_seen[[i]] <- paste(names(formals(sys.function(i))), collapse = ",")
  }
  print(calls)
  print(formals_seen)
  print(sys.parents()[seq_len(n)])
}
restart_names <- function() vapply(computeRestarts(), function(r) paste(r[[1L]], collapse = ""), "")

cat("-- frames inside the expression\n")
tryCatch(calls_here(), error = function(e) NULL)
tryCatch(calls_here(), error = function(e) NULL, warning = function(w) NULL)
tryCatch(calls_here(), finally = cat("finally\n"))
tryCatch(calls_here())
outer <- function() tryCatch(inner(), condition = identity)
inner <- function() calls_here()
outer()

frame_contents <- function() {
  for (i in seq_len(sys.nframe() - 1L)) print(ls(sys.frame(i), all.names = TRUE))
  print(identical(environment(sys.function(2)), sys.frame(1)))
  print(identical(sys.frame(1)$parentenv, globalenv()))
  print(sys.frame(1)$handlers)
  print(sys.frame(2)$nh)
}
tryCatch(frame_contents(), error = function(e) "handler", finally = NULL)
tryCatch(frame_contents(), error = function(e) "e", warning = function(w) "w", message = function(m) "m")

cat("-- frames inside the handler\n")
tryCatch(stop("boom"), error = function(e) calls_here())
tryCatch(warning("careful"), warning = function(w) calls_here())
tryCatch(signalCondition(simpleCondition("tick")), condition = function(c) calls_here())
withCallingHandlers(warning("w"), warning = function(w) {
  calls_here()
  invokeRestart("muffleWarning")
})
suppressWarnings(withCallingHandlers(warning("w"), warning = function(w) calls_here()))

cat("-- the call a caught condition carries\n")
print(tryCatch(stop("x"), error = function(e) conditionCall(e)))
print(tryCatch(warning("x"), warning = function(w) conditionCall(w)))
print(tryCatch(message("x"), message = function(m) conditionCall(m)))
fails <- function(a) stop("in f")
print(tryCatch(fails(1), error = function(e) conditionCall(e)))
print(tryCatch(log(-1), warning = function(w) conditionCall(w)))
print(tryCatch(sqrt("a"), error = function(e) conditionCall(e)))
print(tryCatch(1 + "a", error = function(e) conditionCall(e)))
print(tryCatch(stop("x", call. = FALSE), error = function(e) conditionCall(e)))
print(tryCatch(as.integer("x"), warning = function(w) conditionCall(w)))
print(tryCatch(sys.call(), error = identity))
print(tryCatch(sys.function(), error = identity))
h <- function() tryCatch(stop("inner"), error = function(e) sys.call())
print(h())
print(tryCatch(stop("e"), error = function(e) sys.call(-1)))
print(tryCatch(stop("e"), error = function(e) identical(parent.frame(), globalenv())))
k <- function() tryCatch(stop("e"), error = function(e) environmentName(topenv(parent.frame())))
print(k())

cat("-- nesting and handler order\n")
log_handler <- function(tag) function(c) cat(tag, class(c)[[1]], "\n")
r <- withCallingHandlers(
  tryCatch(
    withCallingHandlers(
      {
        warning("w1")
        "unreached"
      },
      warning = log_handler("inner calling")
    ),
    warning = function(w) {
      cat("exiting", conditionMessage(w), "\n")
      "caught"
    }
  ),
  warning = log_handler("outer calling")
)
print(r)
r <- tryCatch(
  tryCatch(stop("deep"), warning = function(w) "wrong handler"),
  error = function(e) paste("outer got", conditionMessage(e))
)
print(r)
r <- tryCatch(
  tryCatch(stop("deep"), error = function(e) stop("rethrown")),
  error = function(e) paste("outer got", conditionMessage(e))
)
print(r)
r <- tryCatch(stop("first"), error = function(e) "error handler", condition = function(c) "condition handler")
print(r)
r <- tryCatch(stop("first"), condition = function(c) "condition handler", error = function(e) "error handler")
print(r)
r <- tryCatch(warning("w"), error = function(e) "error", warning = function(w) "warning")
print(r)
r <- tryCatch(message("m"), error = function(e) "error", message = function(m) "message")
print(r)
r <- tryCatch(signalCondition(simpleCondition("s")), error = function(e) "error")
print(r)
r <- tryCatch({ warning("first"); stop("second") }, error = function(e) "error", warning = function(w) "warning")
print(r)
r <- withCallingHandlers(
  tryCatch({ message("note"); "body" }, error = function(e) "error"),
  message = function(m) {
    cat("calling saw", conditionMessage(m))
    invokeRestart("muffleMessage")
  }
)
print(r)

cat("-- classes\n")
custom <- function(cls) structure(class = c(cls, "condition"), list(message = paste("custom", cls[[1]]), call = NULL))
r <- tryCatch(stop(custom(c("special_error", "error"))), special_error = function(e) class(e))
print(r)
r <- tryCatch(stop(custom(c("a", "b", "error"))), b = function(e) "matched b", a = function(e) "matched a")
print(r)
r <- tryCatch(signalCondition(custom(c("x", "y"))), y = function(c) conditionMessage(c))
print(r)
r <- tryCatch(signalCondition(custom("z")), y = function(c) "wrong")
print(r)
r <- tryCatch(warning(custom(c("deprecated", "warning"))), deprecated = function(w) class(w))
print(r)
r <- tryCatch(stop(custom(c("a", "error"))), error = function(e) inherits(e, "a"))
print(r)
e1 <- simpleError("plain")
r <- tryCatch(stop(e1), error = function(e) identical(e, e1))
print(r)
r <- tryCatch(stop(e1), condition = function(c) class(c))
print(r)
r <- tryCatch(as.numeric("q"), warning = function(w) class(w))
print(r)
r <- tryCatch(as.numeric("q"), condition = function(c) conditionMessage(c))
print(r)

cat("-- finally on every exit\n")
r <- tryCatch({ cat("body\n"); "normal" }, finally = cat("finally after normal\n"))
print(r)
r <- tryCatch(stop("err"), error = function(e) { cat("handler\n"); "handled" }, finally = cat("finally after handler\n"))
print(r)
r <- tryCatch(
  tryCatch(stop("err"), finally = cat("inner finally\n")),
  error = function(e) { cat("outer handler\n"); "outer" },
  finally = cat("outer finally\n")
)
print(r)
early <- function() {
  tryCatch({ return("returned early"); cat("unreached\n") }, finally = cat("finally after return\n"))
  "fell through"
}
print(early())
for (i in 1:3) {
  tryCatch({ if (i == 2L) break; cat("iteration", i, "\n") }, finally = cat("finally", i, "\n"))
}
for (i in 1:3) {
  tryCatch({ if (i == 2L) next; cat("iteration", i, "\n") }, finally = cat("finally", i, "\n"))
}
r <- withRestarts(
  tryCatch({ invokeRestart("skip", "restarted"); "unreached" }, finally = cat("finally after restart\n")),
  skip = function(value) value
)
print(r)
r <- tryCatch(
  tryCatch(stop("err"), error = function(e) stop("handler failed"), finally = cat("finally after handler error\n")),
  error = function(e) conditionMessage(e)
)
print(r)
r <- tryCatch(tryCatch(1, finally = stop("finally failed")), error = function(e) conditionMessage(e))
print(r)
r <- tryCatch(warning("w"), warning = function(w) "warned", finally = cat("finally after warning\n"))
print(r)
fin <- 0L
r <- tryCatch(10, finally = fin <- fin + 1L)
print(c(r, fin))
r <- tryCatch(invisible(5))
print(withVisible(tryCatch(invisible(5)))$visible)
print(withVisible(tryCatch(invisible(5), error = identity))$visible)
print(withVisible(tryCatch(invisible(5), finally = NULL))$visible)
print(withVisible(tryCatch(stop("x"), error = function(e) invisible(1)))$visible)

cat("-- on.exit interplay\n")
with_exit <- function(fail) {
  on.exit(cat("on.exit of with_exit\n"))
  if (fail) stop("with_exit failed")
  "with_exit ok"
}
print(tryCatch(with_exit(FALSE), error = function(e) "caught", finally = cat("finally\n")))
print(tryCatch(with_exit(TRUE), error = function(e) { cat("handler\n"); "caught" }, finally = cat("finally\n")))
wrapper <- function() {
  on.exit(cat("wrapper on.exit\n"), add = TRUE)
  tryCatch(stop("x"), error = function(e) {
    on.exit(cat("handler on.exit\n"))
    "handler value"
  }, finally = cat("wrapper finally\n"))
}
print(wrapper())
exit_in_expr <- function() {
  tryCatch({
    on.exit(cat("on.exit registered inside expr\n"), add = TRUE)
    "expr value"
  })
}
print(exit_in_expr())
exit_order <- function() {
  on.exit(cat("first\n"))
  tryCatch(on.exit(cat("second\n"), add = TRUE), finally = cat("finally between\n"))
  cat("body end\n")
}
exit_order()

cat("-- suppressWarnings and the muffle restart\n")
r <- suppressWarnings({ warning("a"); warning("b"); "done" })
print(r)
print(suppressWarnings(as.integer(c("1", "x", "3"))))
r <- suppressWarnings(withCallingHandlers(
  { warning("w"); "after" },
  warning = function(w) {
    cat("inner handler saw", conditionMessage(w), "\n")
    print(restart_names())
  }
))
print(r)
r <- withCallingHandlers(
  suppressWarnings({ warning("w"); "after" }),
  warning = function(w) cat("outer handler should not run\n")
)
print(r)
r <- suppressWarnings(withCallingHandlers(
  { warning("w"); "after muffle" },
  warning = function(w) {
    cat("user muffles\n")
    invokeRestart("muffleWarning")
  }
))
print(r)
r <- suppressWarnings({ print(restart_names()); "no warning" })
print(r)
r <- suppressWarnings(withCallingHandlers(warning("w"), warning = function(w) print(restart_names())))
print(r)
r <- suppressWarnings({ message("message passes"); "m" })
print(r)
r <- tryCatch(suppressWarnings(stop("errors pass")), error = function(e) conditionMessage(e))
print(r)
r <- suppressWarnings(warning(custom(c("deprecated", "warning"))), classes = "deprecated")
print(r)
r <- withCallingHandlers(
  suppressWarnings(warning("plain"), classes = "deprecated"),
  warning = function(w) {
    cat("not suppressed:", conditionMessage(w), "\n")
    invokeRestart("muffleWarning")
  }
)
print(r)
print(withVisible(suppressWarnings(invisible(3)))$visible)
f_warns <- function() {
  warning("from f")
  sys.call()
}
print(suppressWarnings(f_warns()))
r <- tryCatch(suppressWarnings(warning("x")), warning = function(w) "escaped")
print(r)
op <- options(warn = 2)
r <- tryCatch(suppressWarnings({ warning("under warn 2"); "ok" }), error = function(e) conditionMessage(e))
print(r)
options(op)
r <- suppressWarnings(tryCatch(warning("inner"), warning = function(w) conditionMessage(w)))
print(r)
r <- suppressWarnings(suppressWarnings({ warning("double"); "double ok" }))
print(r)

cat("-- tryCatch argument errors\n")
r <- tryCatch(tryCatch(1, function(e) 2), error = function(e) conditionMessage(e))
print(r)
r <- tryCatch(1, error = "not a function")
print(r)
r <- tryCatch(tryCatch(stop("x"), error = "not a function"), error = function(e) conditionMessage(e))
print(r)
print(tryCatch(expr = 7))
print(tryCatch(error = function(e) 1, 8))
handlers <- list(error = function(e) "spliced")
print(do.call(tryCatch, c(list(quote(stop("x"))), handlers)))
print(try(stop("try message"), silent = TRUE))
print(class(try(stop("x"), silent = TRUE)))
print(inherits(try(log(1), silent = TRUE), "try-error"))

cat("-- withRestarts and invokeRestart\n")
r <- withRestarts(invokeRestart("myR", 1, 2), myR = function(a, b) a + b)
print(r)
r <- withRestarts({ cat("body\n"); "body value" }, myR = function() "restarted")
print(r)
r <- withRestarts(invokeRestart("second"), first = function() "first", second = function() "second")
print(r)
r <- withRestarts(invokeRestart("described"), described = "a description")
print(r)
rs <- withRestarts(computeRestarts(), alpha = function() 1, beta = "beta description")
print(vapply(rs, function(r) paste(r$name, collapse = ""), ""))
print(vapply(rs[1:2], function(r) is.environment(r$exit), NA))
print(rs[[1]]$description)
print(rs[[2]]$description)
print(rs[[1]]$test(NULL))
print(is.null(rs[[1]]$interactive))
print(names(rs[[1]]))
print(class(rs[[1]]))
print(sort(ls(environment(rs[[1]]$test))))
print(environment(rs[[2]]$handler)$description)
print(environment(rs[[1]]$handler)$name)
withRestarts(calls_here(), here = function() NULL)
frame_names <- function() {
  n <- sys.nframe() - 1L
  lapply(seq_len(n), function(i) ls(sys.frame(i), all.names = TRUE))
}
print(withRestarts(frame_names(), here = function() NULL))
print(withRestarts(frame_names(), here = function() NULL, there = "described"))
h_restart <- function() "from a variable"
print(withRestarts(invokeRestart("hh"), hh = h_restart))
print(withVisible(withRestarts(invokeRestart("v"), v = function() invisible(9)))$visible)
print(withVisible(withRestarts(invokeRestart("v"), v = function() NULL))$visible)
print(tryInvokeRestart("not_there"))
print(withVisible(tryInvokeRestart("not_there"))$visible)
print(tryCatch(invokeRestart("not_there"), error = function(e) conditionMessage(e)))
print(tryCatch(invokeRestart(NA_character_), error = function(e) conditionMessage(e)))
r <- withRestarts(tryInvokeRestart("named", "extra"), named = function(x) paste("got", x))
print(r)
r <- withRestarts(invokeRestart(computeRestarts()[[1]], "obj"), objr = function(x) x)
print(r)
f_restart <- function() {
  on.exit(cat("on.exit before the restart lands\n"))
  invokeRestart("land")
}
print(withRestarts(f_restart(), land = function() "landed"))
r <- withRestarts(withRestarts(invokeRestart("outer_r"), inner_r = function() "inner"), outer_r = function() "outer")
print(r)
print(withRestarts(invokeRestart("noisy"), noisy = function() {
  cat("handler ran\n")
  "noisy"
}))
withRestarts(invokeRestart("stack"), stack = function() calls_here())
print(tryCatch(withRestarts(1, 2), error = function(e) conditionMessage(e)))
print(tryCatch(withRestarts(1, bad = 2), error = function(e) conditionMessage(e)))
r <- withRestarts(invokeRestart("listed"), listed = list(handler = function() "from a list spec"))
print(r)
r <- withCallingHandlers(
  { warning("w"); "value after muffle" },
  warning = function(w) tryInvokeRestart("muffleWarning")
)
print(r)
r <- withCallingHandlers(
  { message("m"); "value after message muffle" },
  message = function(m) invokeRestart("muffleMessage")
)
print(r)
r <- suppressMessages({ message("quiet"); "suppressed message" })
print(r)
