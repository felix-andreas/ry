log <- character()
note <- function(...) log <<- c(log, paste0(...))
show <- function(value) { print(value); print(log); log <<- character() }

# A frame that returns drops what its exit code registered.
f1 <- function() { on.exit({ note("a"); on.exit(note("added in handler")); note("b") }); note("body"); 1 }
show(f1())
f2 <- function() { on.exit({ note("a"); on.exit(note("added"), add = TRUE); note("b") }); on.exit(note("second"), add = TRUE); 1 }
show(f2())
f3 <- function() { on.exit({ note("a"); on.exit(note("added"), add = TRUE, after = FALSE) }); on.exit(note("second"), add = TRUE); 1 }
show(f3())
f4 <- function() { on.exit({ note("a"); on.exit(); note("cleared") }); on.exit(note("second"), add = TRUE); 1 }
show(f4())
f5 <- function() { on.exit({ on.exit(note("replaced")); note("first") }); on.exit(note("dropped"), add = TRUE); 1 }
show(f5())
f6 <- function() { for (i in 1:3) on.exit({ note("h"); on.exit(note("x"), add = TRUE) }, add = TRUE); 1 }
show(f6())

# Exit code that leaves by a jump runs what it registered.
f7 <- function() { on.exit({ note("a"); on.exit(note("added")); return(invisible(2)) }); note("body"); 1 }
show(f7())
f8 <- function() { on.exit({ note("a"); on.exit(note("after error")); stop("in handler") }); 1 }
show(tryCatch(f8(), error = function(e) conditionMessage(e)))
f9 <- function() {
    on.exit({ note("a"); on.exit(note("added"), add = TRUE); stop("first fails") })
    on.exit(note("second"), add = TRUE)
    1
}
show(tryCatch(f9(), error = function(e) conditionMessage(e)))
f10 <- function() {
    on.exit({ note("a"); on.exit({ note("b"); on.exit(note("c")); return("from the second round") }); return("from the first round") })
    1
}
show(f10())

# A frame that a jump is unwinding runs what its last exit code registered.
f11 <- function() { on.exit({ note("a"); on.exit(note("added")); note(deparse(sys.on.exit())) }); stop("body fails") }
show(tryCatch(f11(), error = function(e) conditionMessage(e)))
f12 <- function() {
    on.exit({ note("a"); on.exit({ note("b"); on.exit(note("c")) }) })
    stop("body fails")
}
show(tryCatch(f12(), error = function(e) conditionMessage(e)))
f13 <- function() {
    on.exit({ note("a"); on.exit(note("dropped")) })
    on.exit({ note("b"); on.exit(note("kept")) }, add = TRUE)
    stop("body fails")
}
show(tryCatch(f13(), error = function(e) conditionMessage(e)))
f14 <- function() {
    on.exit({ note("a"); on.exit(return("rescued")) })
    stop("body fails")
}
show(tryCatch(f14(), error = function(e) conditionMessage(e)))
f15 <- function() {
    on.exit({ note("a"); on.exit(note("added")) })
    invokeRestart("leave", "through a restart")
}
show(withRestarts(f15(), leave = function(value) value))
f16 <- function(n) {
    on.exit({ note("exit ", n); on.exit(note("more ", n)) })
    if (n > 0) f16(n - 1) else stop("at the bottom")
}
show(tryCatch(f16(2), error = function(e) conditionMessage(e)))

# The same closures a second time, and compiled.
show(f7())
show(tryCatch(f11(), error = function(e) conditionMessage(e)))
g <- compiler::cmpfun(f1)
show(g())
g <- compiler::cmpfun(f12)
show(tryCatch(g(), error = function(e) conditionMessage(e)))
show(tryCatch(g(), error = function(e) conditionMessage(e)))

# Exit code registered from another frame or through eval.
f17 <- function() { on.exit({ note("a"); eval(quote(on.exit(note("via eval")))); note("b") }); stop("body fails") }
show(tryCatch(f17(), error = function(e) conditionMessage(e)))
f18 <- function() { on.exit({ h <- function() on.exit(note("inner frame"), add = TRUE); h(); note("b"); print(sys.on.exit()) }); 1 }
show(f18())
f19 <- function() {
    on.exit({ note("a"); do.call(on.exit, list(quote(note("via do.call")), TRUE), envir = environment()); note(deparse(sys.on.exit())) })
    stop("body fails")
}
show(tryCatch(f19(), error = function(e) conditionMessage(e)))
