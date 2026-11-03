# lengths() dispatches on length() methods and is itself an internal generic.
length.box <- function(x) 7L
lengths.box <- function(x, use.names = TRUE) c(a = 1L)
b <- structure(list(1, 2), class = "box")
print(lengths(b))
print(lengths(list(structure(list(1), class = "box"), 1:3)))
r <- tryCatch(lengths(quote(f)), error = conditionMessage)
print(r)

# NextMethod from an internal generic falls back to the .Internal code.
rep_len.foo <- function(x, ...) structure(NextMethod(), class = "foo")
x <- structure(1:3, class = "foo")
print(unclass(rep_len(x, 5)))

# Ops group dispatch drops argument names before calling the method.
Ops.bar <- function(e1, e2) paste(.Generic, missing(e2))
y <- structure(1, class = "bar")
print(get("*")(x = y, y = 3))
print(mapply(get("+"), x = list(y), y = 2))
