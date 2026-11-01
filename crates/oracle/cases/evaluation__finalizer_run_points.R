# A finalizer runs at the first gc() after its object becomes unreachable, even when that gc()
# is inside the same braces, inside a promise, or in a compiled closure (the fourth call on).
make <- function(tag) {
  object <- new.env()
  reg.finalizer(object, function(x) cat("finalized", tag, "\n"))
  object
}
reassigned <- function(tag) { held <- make(tag); { held <- NULL; invisible(gc()) }; cat("after", tag, "\n") }
for (tag in 1:4) reassigned(tag)
removed <- function(tag) { held <- make(tag); rm(held); invisible(gc()); cat("after", tag, "\n") }
for (tag in 5:8) removed(tag)
forcing <- function(code) code
in_promise <- function(tag) { held <- make(tag); forcing({ rm(held); invisible(gc()) }); cat("after", tag, "\n") }
for (tag in 9:10) in_promise(tag)
