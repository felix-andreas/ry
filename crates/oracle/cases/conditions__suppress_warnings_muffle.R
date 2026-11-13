# suppressWarnings muffles a warning natively when its own handler is the topmost one that matches;
# every case here must look exactly as if the R handler and the muffleWarning restart had run.
show <- function(label, expr) {
  cat("--", label, "\n")
  str(withVisible(tryCatch(expr, error = function(e) paste("error:", conditionMessage(e)))))
}

show("coercion", suppressWarnings(as.integer("a")))
show("coercions", suppressWarnings(c(as.numeric("x"), sqrt(-1), log(-1))))
show("warning value", suppressWarnings(warning("x")))
show("nested visibility", withVisible(suppressWarnings(warning("x"))))

outer_muffle <- function(w) {
  cat("outer saw:", conditionMessage(w), "\n")
  invokeRestart("muffleWarning")
}
show("classes foo", withCallingHandlers(suppressWarnings(warning("x"), classes = "foo"), warning = outer_muffle))
show("classes foo, warning", suppressWarnings(warning("x"), classes = c("foo", "warning")))
show("classes condition", suppressWarnings(warning("x"), classes = "condition"))
show("classes simpleWarning", suppressWarnings(as.integer("b"), classes = "simpleWarning"))
show("classes message", withCallingHandlers(suppressWarnings(warning("y"), classes = "message"), warning = outer_muffle))
force_classes <- function() {
  cat("classes forced\n")
  "warning"
}
show("promise classes", suppressWarnings(warning("p"), classes = force_classes()))
cls <- "warning"
show("variable classes", suppressWarnings(warning("v"), classes = cls))

custom <- structure(class = c("myWarning", "warning", "condition"), list(message = "m", call = NULL))
show("custom class", withCallingHandlers(suppressWarnings(warning(custom), classes = "myWarning"), warning = outer_muffle))
show("custom class, plain warning", withCallingHandlers(suppressWarnings(warning("plain"), classes = "myWarning"), warning = outer_muffle))

show("lookalike handler", withCallingHandlers(warning("look"), warning = function(w) {
  cat("lookalike ran\n")
  invokeRestart("muffleWarning")
}))
show("handler outside", withCallingHandlers(suppressWarnings(warning("in")), warning = function(w) cat("outer handler ran\n")))
show("handler inside", suppressWarnings(withCallingHandlers(warning("in2"), warning = function(w) cat("inner handler ran\n"))))
show("tryCatch inside", suppressWarnings(tryCatch(warning("tc"), warning = function(w) "caught")))
show("tryCatch outside", tryCatch(suppressWarnings(warning("tc2")), warning = function(w) "caught"))
show("warning in a handler", suppressWarnings(withCallingHandlers(message("m"), message = function(m) {
  warning("from handler")
  cat("handler continues\n")
  invokeRestart("muffleMessage")
})))
show("warning in an outer handler", suppressWarnings(withCallingHandlers(warning("first"), warning = function(w) {
  warning("second")
  cat("outer handler continues\n")
})))

op <- options(warn = 2)
show("warn 2 coercion", suppressWarnings(as.integer("c")))
show("warn 2 warning", suppressWarnings(warning("w2")))
options(op)
op <- options(warn = 1)
show("warn 1", suppressWarnings(warning("w1")))
options(op)

f <- function() {
  suppressWarnings(warning("inside f"))
  sys.call()
}
show("in a function", f())
show("frames", suppressWarnings({
  warning("d")
  length(sys.calls())
}))
suppressWarnings(warning("lw"))
cat("last.warning exists:", exists("last.warning", envir = baseenv()), "\n")
print(warnings())
g <- function() {
  suppressWarnings(as.integer("z"))
  warning("after")
  1
}
show("later warning", g())

hot <- function(x) suppressWarnings(as.integer(x))
total <- 0L
for (i in 1:3000) total <- total + is.na(hot(if (i %% 2L) "a" else "1"))
cat("hot loop:", total, "\n")

local({
  suppressWarnings <- function(expr, classes = "warning") {
    withCallingHandlers(expr, warning = function(w) {
      cat("local redefinition ran\n")
      invokeRestart("muffleWarning")
    })
  }
  show("local redefinition", suppressWarnings(warning("r")))
})
original <- base::suppressWarnings
unlockBinding("suppressWarnings", baseenv())
assign("suppressWarnings", function(expr, classes = "warning") {
  withCallingHandlers(expr, warning = function(w) {
    cat("base redefinition ran\n")
    invokeRestart("muffleWarning")
  })
}, envir = baseenv())
show("base redefinition", suppressWarnings(warning("b")))
show("original after redefinition", original(warning("o")))
assign("suppressWarnings", original, envir = baseenv())
lockBinding("suppressWarnings", baseenv())
show("restored", suppressWarnings(warning("restored")))
