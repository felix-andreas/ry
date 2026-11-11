# oracle: requires rlang, bit64
# A compiled closure that calls a base function checks only that nothing rebound the name, so
# every route that can bind a name outside the base frames must make the next call see it. Each
# block warms a compiled caller, rebinds the operator by one route, calls again, and undoes the
# rebinding. GNU R's compiler binds base functions when it compiles a closure and ignores later
# shadowing, so its JIT is off here; ry's compiled code must match R's evaluator.
invisible(compiler::enableJIT(0))
warm <- function(f, ...) { for (i in 1:3) f(...); f(...) }

add <- function(a, b) a + b
index <- function(x) x[2]
size <- function(x) length(x)
paren <- function(x) (x)
nested <- local({ outer <- function() function(a, b) a + b; outer() })
print(c(warm(add, 1, 2), warm(index, 4:6), warm(size, 1:7), warm(paren, 9), warm(nested, 1, 2)))

# `<-` and `=` at top level, then removal.
`+` <- function(a, b) "global plus"
print(add(1, 2)); print(nested(1, 2))
rm(`+`)
print(add(1, 2))
length = function(x) "global length"
print(size(1:3))
rm(length)
print(size(1:3))

# `<<-` from a function that was itself compiled before it ran. It reaches a base name only
# through an existing binding, since assigning into base itself fails on the locked binding.
set_global <- function(value) `[` <<- value
`[` <- NULL
for (i in 1:3) set_global(NULL)
print(index(4:6))
set_global(function(x, i) "superassigned index")
print(index(4:6))
rm(`[`)
print(index(4:6))

# assign(), delayedAssign() and makeActiveBinding() into the caller's environment.
e <- new.env()
in_e <- function(a, b) a + b
environment(in_e) <- e
print(warm(in_e, 1, 2))
assign("+", function(a, b) "assigned", envir = e)
print(in_e(1, 2)); rm("+", envir = e)
delayedAssign("+", function(a, b) "delayed", assign.env = e)
print(in_e(1, 2)); rm("+", envir = e)
makeActiveBinding("+", function() function(a, b) "active", e)
print(in_e(1, 2)); rm("+", envir = e)
print(in_e(1, 2))

# list2env(), and the `$<-` and `[[<-` forms on an environment.
invisible(list2env(list(`+` = function(a, b) "listed"), e))
print(in_e(1, 2)); rm("+", envir = e)
e$`+` <- function(a, b) "dollar"
print(in_e(1, 2)); rm("+", envir = e)
e[["+"]] <- function(a, b) "double bracket"
print(in_e(1, 2)); rm("+", envir = e)
# A binding that is not a function does not hide the base one from a call.
e$`+` <- 17
print(in_e(1, 2)); rm("+", envir = e)

# attach() and detach(), and sys.source() and eval() into an environment.
attach(list(`(` = function(x) "attached paren"), name = "shadowing")
print(paren(9))
detach("shadowing")
print(paren(9))
source_file <- tempfile(fileext = ".R")
writeLines("`+` <- function(a, b) 'sourced'", source_file)
sys.source(source_file, envir = e)
print(in_e(1, 2)); rm("+", envir = e)
source(source_file, local = e)
print(in_e(1, 2)); rm("+", envir = e)
eval(quote(`+` <- function(a, b) "evaluated"), e)
print(in_e(1, 2)); rm("+", envir = e)

# Rebinding inside a function that was compiled and ran before, after its nested closure ran.
maker <- function(shadow) {
  inner <- function(a, b) a + b
  first <- inner(1, 2)
  if (shadow) `+` <- function(a, b) "local plus"
  c(first, inner(1, 2))
}
for (i in 1:3) maker(FALSE)
print(maker(FALSE)); print(maker(TRUE))
late <- function() function(a, b) a + b
inner <- late()
print(warm(inner, 1, 2))
assign("+", function(a, b) "into a function frame", envir = environment(inner))
print(inner(1, 2))

# Rebinding the base binding itself.
old_size <- size(1:3)
unlockBinding("length", baseenv())
assign("length", function(x) "base rebound", envir = baseenv())
print(size(1:3))
assign("length", .Primitive("length"), envir = baseenv())
lockBinding("length", baseenv())
print(c(old_size, size(1:3)))

# The C API: Rf_defineVar, R_MakeActiveBinding and a lazy binding through rlang's .Call entries.
rlang::env_bind(e, `+` = function(a, b) "defineVar")
print(in_e(1, 2)); rm("+", envir = e)
rlang::env_bind_active(e, `+` = function() function(a, b) "active from C")
print(in_e(1, 2)); rm("+", envir = e)
rlang::env_bind_lazy(e, `+` = function(a, b) "lazy from C")
print(in_e(1, 2)); rm("+", envir = e)
print(in_e(1, 2))

# Loading and attaching a namespace that masks a base function.
span <- function(a, b) a:b
print(warm(span, 1L, 3L))
suppressPackageStartupMessages(library(bit64))
print(class(span(as.integer64(1), 3L)))
detach("package:bit64")

# A closure whose environment chain no longer reaches base finds no base function.
lone <- function(a, b) a + b
print(warm(lone, 1, 2))
environment(lone) <- emptyenv()
print(tryCatch(lone(1, 2), error = conditionMessage))
detached <- new.env()
far <- function(a, b) a + b
environment(far) <- detached
print(warm(far, 1, 2))
parent.env(detached) <- emptyenv()
print(tryCatch(far(1, 2), error = conditionMessage))
parent.env(detached) <- globalenv()
print(far(1, 2))
# Reparenting an environment that frames already descend from detaches them too.
grand <- new.env()
make_child <- function() { function(a, b) a + b }
environment(make_child) <- grand
child <- make_child()
print(warm(child, 1, 2))
parent.env(grand) <- emptyenv()
print(tryCatch(child(1, 2), error = conditionMessage))
parent.env(grand) <- globalenv()
print(c(child(1, 2), add(1, 2), index(4:6), size(1:7), paren(9)))
