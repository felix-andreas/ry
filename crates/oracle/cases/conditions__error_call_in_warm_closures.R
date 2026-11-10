# The call an error reports when a closure has run often enough to take the compiled path: an
# unbound variable is reported against the closure's call on every call, not only the first ones.
# Found by `oracle fuzz --seed 24`.
k <- function(n) nchar(undefined_text)
for (r in 1:12) print(tryCatch(k(2), error = function(e) deparse(conditionCall(e))))
# The variable is read directly, inside a loop, through a local function, and from a promise.
direct <- function() undefined_text
looped <- function() { for (i in 1:3) x <- undefined_text; x }
outer <- function() { inner <- function() undefined_text; inner() }
lazy <- function(x) x
for (r in 1:6) {
    print(tryCatch(direct(), error = function(e) deparse(conditionCall(e))))
    print(tryCatch(looped(), error = function(e) deparse(conditionCall(e))))
    print(tryCatch(outer(), error = function(e) deparse(conditionCall(e))))
    print(tryCatch(lazy(undefined_text), error = function(e) deparse(conditionCall(e))))
}
# A closure whose frame was given an environment, and one that reads a variable it later defines.
reified <- function() { environment(); undefined_text }
late <- function() { if (FALSE) undefined_text <- 1; undefined_text }
for (r in 1:4) {
    print(tryCatch(reified(), error = function(e) conditionMessage(e)))
    print(tryCatch(late(), error = function(e) deparse(conditionCall(e))))
}
# The message an uncaught error prints names the call too.
for (r in 1:3) try(k(r))
