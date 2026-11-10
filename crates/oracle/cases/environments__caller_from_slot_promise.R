# Promise code that runs on a slot frame's slots calls from below the frame that forces it,
# so `sys.parent()` must name the slot frame, not the frame below the callee.
id <- function(v) v
p <- function(v) list(sys.call(sys.parent()), substitute(v), sys.function(sys.parent()))
h <- function(type = 1) id(p(type))
for (i in 1:50) r <- h(1)
str(h(2)[1:2])
identical(h(3)[[3]], h)
g <- function(type = c("gzip", "xz", "none")) match(match.arg(type), c("none", "gzip", "xz"))
for (i in 1:50) r <- g("xz")
g("none")
pick <- function(arg) formals(sys.function(sys.parent()))[[as.character(substitute(arg))]]
k <- function(mode = c("a", "b")) id(pick(mode))
k("b")
