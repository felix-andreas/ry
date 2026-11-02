# Repeated S3 dispatch must see methods that appear, change, or disappear between calls.
describe <- function(x) UseMethod("describe")
describe.default <- function(x) "default"
obj <- structure(1, class = "thing")
print(describe(obj))
describe.thing <- function(x) "thing v1"
print(describe(obj))
describe.thing <- function(x) "thing v2"
print(describe(obj))
describe.thing <- 42
print(describe(obj))
describe.thing <- function(x) "thing v3"
print(describe(obj))
rm(describe.thing)
print(describe(obj))

# A method on an attached environment, found through the search path.
attached <- new.env()
assign("describe.thing", function(x) "attached", envir = attached)
attach(attached, name = "methods_cache_test")
print(describe(obj))
detach("methods_cache_test")
print(describe(obj))

# A registered method, then a method defined in the calling function's frame.
registerS3method("describe", "thing", function(x) "registered", envir = environment(describe))
print(describe(obj))
local({
  describe.thing <- function(x) "local"
  print(describe(obj))
})
print(describe(obj))

# Base generics dispatching in a loop, with a method added midway.
values <- character()
for (i in 1:4) {
  if (i == 3) format.thing <- function(x, ...) "formatted thing"
  values[i] <- format(obj)
}
print(values)

# Replacing the global methods table, which reassigns an existing binding.
.__S3MethodsTable__. <- new.env()
rm(format.thing)
other <- structure(1, class = "other")
print(describe(other))
late <- new.env()
assign("describe.other", function(x) "late table", envir = late)
.__S3MethodsTable__. <- late
print(describe(other))

# A frame chain that ends at the empty environment continues at base, not the global frame.
.__S3MethodsTable__. <- new.env()
describe.other <- function(x) "global other"
print(tryCatch(
  eval(as.call(list(describe, other)), new.env(parent = emptyenv())),
  error = conditionMessage
))
print(describe(other))
