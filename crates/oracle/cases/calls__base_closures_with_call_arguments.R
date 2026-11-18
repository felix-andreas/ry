# Base closures that ry runs natively when their arguments are plain values (`isTRUE`,
# `identical`, `unname`, `nrow`, `%in%`, `%||%`, `inherits` and others), called with arguments
# that are calls, from sites that run often enough to plan.
same <- function(x) x
checks <- function(n) {
  out <- list()
  frame <- data.frame(a = 1:3, b = letters[1:3])
  for (i in seq_len(n)) {
    out[[i]] <- list(
      isTRUE(same(i > 3L)),
      isFALSE(same(i > 3L)),
      identical(same(i), 4L),
      identical(c(a = i), c(a = same(i))),
      unname(c(a = i, b = same(i))),
      nrow(frame[seq_len(i %% 3L + 1L), ]),
      ncol(cbind(frame, i)),
      same(i) %in% c(2L, 5L),
      match(same(i), 6:1),
      NULL %||% same(i),
      typeof(same(i) / 2),
      inherits(structure(i, class = "tagged"), "tagged"),
      is.factor(factor(letters[i])),
      is.data.frame(same(frame)),
      is.vector(same(i), "integer"),
      getOption(paste0("no.such.option.", i), same(i))
    )
  }
  out
}
str(checks(6L))

# The base closure's frame is on the stack while its argument runs.
frames <- function() sys.nframe()
calls <- function() vapply(sys.calls(), function(call) deparse(call[[1L]])[1L], "")
watching <- function(n) {
  out <- list()
  for (i in seq_len(n)) out[[i]] <- list(isTRUE(frames() > 2L), identical(frames(), 3L), unname(calls()))
  out[[n]]
}
str(watching(6L))

# Arguments stay lazy, and one that fails names the right call.
lazy <- function(n) {
  out <- character(0)
  for (i in seq_len(n)) {
    out <- c(out, i %||% stop("never run"))
    out <- c(out, tryCatch(isTRUE(stop("in the argument ", i)), error = function(e) {
      paste(conditionMessage(e), deparse(conditionCall(e)))
    }))
    out <- c(out, tryCatch(identical(same(i)), error = function(e) conditionMessage(e)))
    out <- c(out, tryCatch(nrow(same()), error = function(e) conditionMessage(e)))
  }
  out
}
print(lazy(5L))

# `x <- unname(x)` and its like, with the target as the argument or inside a call.
rebinding <- function(n) {
  value <- c(a = 1L, b = 2L)
  flag <- TRUE
  kind <- "start"
  for (i in seq_len(n)) {
    value <- unname(value)
    value <- unname(c(value, i))
    flag <- isTRUE(flag)
    flag <- isTRUE(same(flag) && i < 4L)
    kind <- typeof(kind)
    kind <- typeof(same(kind))
  }
  list(value, flag, kind)
}
str(rebinding(6L))

# A user's function of the same name takes the call once it is defined.
masked <- function(n) {
  out <- character(0)
  for (i in seq_len(n)) {
    if (i == 4L) isTRUE <- function(x) paste("mine", x)
    out <- c(out, as.character(isTRUE(same(i > 1L))))
  }
  out
}
print(masked(6L))

# Dispatch on a classed argument that a call makes, and a copy of a base closure.
tagged <- function(i) structure(list(i), class = "tagged")
dim.tagged <- function(x) c(x[[1L]], 2L)
`%in%` <- base::`%in%`
classed <- function(n) {
  out <- list()
  for (i in seq_len(n)) out[[i]] <- list(nrow(tagged(i)), ncol(tagged(i)), inherits(tagged(i), c("other", "tagged"), which = TRUE), "tagged" %in% class(tagged(i)))
  out[[n]]
}
str(classed(5L))

# A functional calls closures whose defaults a slot frame cannot hold as they are: one that
# reads `...`, one that makes a function, one that reads the frame, and one that fails.
dotted <- function(x, ..., y = ..1) if (...length() > 0L) y else x
maker <- function(x, f = function() x * 2L) f()
framed <- function(x, e = environment()) sort(ls(e))
fragile <- function(x, y = stop("no default for y")) if (x > 2L) y else x
print(vapply(1:3, dotted, 0L))
print(vapply(1:3, dotted, 0, 0.5))
print(vapply(1:3, maker, 0L))
print(lapply(1:2, framed))
print(vapply(1:2, fragile, 0L))
print(tryCatch(lapply(1:4, fragile), error = function(e) conditionMessage(e)))
print(Map(function(x, y = x + 1L, z = y * 2L) c(x, y, z), 1:3))
print(mapply(function(x, y = x + 1L) x + y, 1:3, c(10L, 20L, 30L)))
print(do.call(maker, list(4L)))
print(Reduce(function(total, x, weight = 2L) total + x * weight, 1:5, 0L))
print(Filter(function(x, limit = 3L) x > limit, 1:6))
