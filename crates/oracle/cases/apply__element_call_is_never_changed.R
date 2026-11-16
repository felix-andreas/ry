# `lapply` and `vapply` run `FUN(X[[i]], ...)` for each element. A callee that changes the call
# it sees, or the code of its argument, changes its own copy.
show <- function(label, value) cat(label, ":", deparse(value), "\n")

edits <- function(x) {
  call <- sys.call()
  call[[1]] <- quote(renamed)
  call[[2]][[3]] <- 99L
  code <- substitute(x)
  code[[2]] <- quote(Y)
  code[[1]] <- as.name("[")
  list(call, code)
}
for (round in 1:3) {
  show("edited", lapply(1:2, edits))
  show("call", lapply(1:2, function(x) sys.call()))
  show("code", vapply(1:2, function(x) deparse(substitute(x)), ""))
  show("matched", lapply(1:2, function(x, ...) match.call(), extra = round))
}

# The call of an error raised in `FUN`.
for (round in 1:3) {
  failed <- tryCatch(lapply(1:2, function(x) stop("no")), error = function(e) conditionCall(e))
  failed[[1]] <- quote(changed)
  failed[[2]] <- NULL
  show("error call edited", failed)
  show("error call", tryCatch(vapply(1:2, function(x) stop("no"), 0),
                              error = function(e) conditionCall(e)))
}

# Attributes set on the call.
tagged <- function(x) {
  call <- sys.call()
  attr(call, "note") <- "mine"
  attributes(call)
}
show("tagged", lapply(1:2, tagged))
show("untagged", lapply(1:2, function(x) attributes(sys.call())))

# The argument's code, kept and changed after the loop.
kept <- lapply(1:2, function(x) substitute(x))
kept[[1]][[3]] <- quote(j)
show("kept", kept)
show("after", lapply(1:2, function(x) substitute(x)))

# A loop inside a loop, and every caller's call.
show("nested", lapply(1:2, function(a) vapply(1:2, function(b) deparse(sys.call()), "")))
show("calls", lapply(1, function(a) lapply(1, function(b) vapply(sys.calls(), function(call) deparse(call)[1], ""))))

# `Map`, `sapply` and `Filter` go through the same loop.
show("sapply", sapply(1:2, function(x) deparse(sys.call())))
show("filter", Filter(function(x) { call <- sys.call(); call[[1]] <- quote(g); x > 1 }, 1:3))
show("after all", lapply(1:2, function(x) sys.call()))
