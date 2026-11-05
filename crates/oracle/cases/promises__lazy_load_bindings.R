# makeLazy binds each name to a promise whose code is the expression with its first argument
# replaced by that name's key, evaluated in the shared environment.
db <- new.env()
db$offset <- 10
target <- new.env()
.Internal(makeLazy(c("a", "b"), list(c(1, 2), c(3, 4)), quote(sum(key, offset)), db, target))
substitute(a, target)
substitute(b, target)
get("b", envir = target)
get("a", envir = target)
# Each promise is forced once.
db$offset <- 100
get("a", envir = target)
