# Base R behaviors that package code depends on.

# Forcing a promise in compiled code keeps its visibility.
f <- function(x) { 1; x }
f(invisible(1))
f(invisible(2))
f(invisible(3))
withCallingHandlers(invisible(4))

# `switch` forwards the caller's alternatives through `...`.
pick <- function(x, ...) switch(x, ...)
pick("b", a = "first", b = "second")
pick("z", a = 1, 2)
pick("a", a = , b = "fell through")

# `topenv()` of a non-environment is base's namespace.
topenv(sum)
topenv(function() 1)

# Non-syntactic names deparse in backticks, except where `backtick = FALSE`.
dput(c("a b" = 1))
dput(list("# x" = 1))
dput(structure(1, "my attr" = 2))
deparse(c("a b" = 1))
deparse(list("a b" = 1), backtick = TRUE)
