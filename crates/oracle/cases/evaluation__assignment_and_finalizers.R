# A string in function position parses as a symbol.
class(quote("f<-"(x))[[1]])
identical(quote("f<-"(x, value = 1)), quote(`f<-`(x, value = 1)))

# [[<- and $<- on NULL build a list; $<- on an empty vector coerces it.
x <- NULL; x[[1]] <- 1; dput(x)
x <- NULL; x[[2]] <- "s"; dput(x)
x <- NULL; x$a <- 1:3; dput(x)
x <- NULL; x["a"] <- 1; dput(x)
x <- integer(); x$a <- 1; dput(x)

# all() and any() skip empty lists.
all(list())
any(list(), FALSE)

# Builtin replacement functions report against the whole assignment.
f <- function() { x <- 1:4; x[1:2] <- 1:3; x }
f()

# return() inside on.exit sets the value.
r <- function() { on.exit(return("from on.exit")); "from body" }
r()

# Nested print() calls get only the arguments the caller supplied.
print.marker <- function(x, ...) cat("marker args:", names(list(...)), "\n")
l <- list(a = structure(1, class = "marker"))
print(l)
print(l, digits = 3)

# A finalizer runs once its object is unreachable and gc() runs.
flag <- new.env(); flag$done <- FALSE
g <- function() { e <- new.env(); reg.finalizer(e, function(x) flag$done <- TRUE); rm(e); invisible(gc()); flag$done }
g()
