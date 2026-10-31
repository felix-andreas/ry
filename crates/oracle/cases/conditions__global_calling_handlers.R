globalCallingHandlers(warning = function(w) {
  cat("global handler saw:", conditionMessage(w), "\n")
  invokeRestart("muffleWarning")
})
f <- function() { warning("from f"); "f done" }
print(f())
withCallingHandlers(
  warning("inner"),
  warning = function(w) cat("local handler saw:", conditionMessage(w), "\n")
)
h <- globalCallingHandlers()
print(names(h))
globalCallingHandlers(NULL)
print(length(globalCallingHandlers()))
