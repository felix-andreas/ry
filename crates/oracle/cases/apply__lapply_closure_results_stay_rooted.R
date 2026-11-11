# Each call `lapply` makes binds its element through a promise that leaves the argument pool, so
# that promise must stay rooted while the call allocates. A collection inside the call (most
# visibly under GC stress) must not free it, whichever way the closure binds its arguments.
x <- lapply(1:20, function(i) paste0("a", i))
print(unlist(x))
generic <- function(i) {
  if (FALSE) .Generic
  paste0("g", i, collapse = "")
}
print(unlist(lapply(1:20, generic)))
dots <- function(i, ...) paste0("d", i, ...)
print(unlist(lapply(1:20, dots, "z")))
print(vapply(1:20, function(i) nchar(paste0("v", i)), integer(1)))
