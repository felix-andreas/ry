# S3 method lookup reads the calling frames, the top environment, the generic's methods table
# and base, and each dispatch must see a method wherever and whenever it appears. The method
# names are fresh, so the first dispatch of each runs while nothing but base binds it.
probe <- function(x, ...) UseMethod("probe")
probe.default <- function(x, ...) "default"
item <- structure(1, class = c("lkA", "lkB"))

# A method in the calling frame, in the global frame, and in the methods table.
print(probe(item))
local({
  probe.lkB <- function(x, ...) "frame lkB"
  print(probe(item))
})
probe.lkB <- function(x, ...) "global lkB"
print(probe(item))
registerS3method("probe", "lkA", function(x, ...) "table lkA", envir = environment(probe))
print(probe(item))
from_frame <- function() {
  probe.lkA <- function(x, ...) "frame lkA"
  probe(item)
}
print(from_frame())
print(probe(item))

# An argument named like a method is a binding of the calling frame.
takes_method <- function(probe.lkC) probe(structure(1, class = "lkC"))
print(takes_method(function(x, ...) "argument lkC"))
print(probe(structure(1, class = "lkC")))

# registerS3method after a builtin generic dispatched to its default.
counted <- structure(1:3, class = "lkCount")
print(length(counted))
registerS3method("length", "lkCount", function(x) 99L)
print(length(counted))
print(length(unclass(counted)))

# A method a later assignment shadows, in the global frame and in a function frame.
format_twice <- function(x) {
  first <- format(x)
  format.lkShadow <- function(x, ...) "shadowed in frame"
  c(first, format(x))
}
shadowed <- structure(2, class = "lkShadow")
print(format_twice(shadowed))
format.lkShadow <- function(x, ...) "global shadow"
print(format_twice(shadowed))
format.lkShadow <- function(x, ...) "global shadow again"
print(format(shadowed))
rm(format.lkShadow)
print(format(shadowed))

# Implicit classes, with methods defined after the first dispatch of each.
shapes <- list(matrix(1:4, 2), array(1:8, c(2, 2, 2)), 1L, 1.5, sum, function(x) x)
print(vapply(shapes, probe, ""))
probe.matrix <- function(x, ...) "matrix"
probe.array <- function(x, ...) "array"
probe.integer <- function(x, ...) "integer"
probe.numeric <- function(x, ...) "numeric"
probe.function <- function(x, ...) "function"
print(vapply(shapes, probe, ""))
rm(probe.integer, probe.matrix)
print(vapply(shapes, probe, ""))

# NextMethod to methods that appear after the first call, and the dispatch variables.
chain <- structure(1, class = c("lkOne", "lkTwo", "lkThree"))
probe.lkOne <- function(x, ...) {
  c(paste("one", .Generic, paste(.Class, collapse = "/"), .Method), NextMethod())
}
print(probe(chain))
probe.lkThree <- function(x, ...) c(paste("three", paste(.Class, collapse = "/")), NextMethod())
print(probe(chain))
probe.lkTwo <- function(x, ...) c(paste("two", .Method), NextMethod())
print(probe(chain))
print(attr(local({
  probe.lkTwo <- function(x, ...) .Class
  probe(structure(1, class = c("lkZero", "lkTwo")))
}), "previous"))

# A methods table detached from base, which a registration into marks nothing.
table_generic <- function(x) UseMethod("table_generic")
table_generic.default <- function(x) "default"
detached_table <- new.env(parent = emptyenv())
.__S3MethodsTable__. <- detached_table
assign("table_generic.lkTable", function(x) "detached table", envir = detached_table)
print(table_generic(structure(1, class = "lkTable")))
rm(.__S3MethodsTable__.)

# A method bound while its frame was detached, found once the frame is attached.
holder <- new.env(parent = emptyenv())
assign("probe.lkHeld", function(x, ...) "held", envir = holder)
caller <- function() probe(structure(1, class = "lkHeld"))
environment(caller) <- holder
print(tryCatch(caller(), error = conditionMessage))
parent.env(holder) <- globalenv()
print(caller())

# Repeated dispatch in a loop while a method comes and goes.
seen <- character()
looped <- structure(1, class = "lkLoop")
for (i in 1:6) {
  if (i == 3) probe.lkLoop <- function(x, ...) "loop method"
  if (i == 5) rm(probe.lkLoop)
  seen[i] <- probe(looped)
}
print(seen)
