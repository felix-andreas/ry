# match.arg reads a missing `choices` from the calling function's formal default, evaluated in
# the caller's frame, so a `c` that frame can see must be the one that runs.
f <- function(type = c("linear", "quadratic")) match.arg(type)
print(f()); print(f("quad"))

# `c` masked by a local, by a formal, by the global environment and by an attached environment.
local_c <- function(type = c("linear", "quadratic")) {
  c <- function(...) toupper(base::c(...))
  match.arg(type)
}
print(local_c()); print(tryCatch(local_c("linear"), error = conditionMessage)); print(local_c("QUAD"))
formal_c <- function(type = c("one", "two"), c = function(...) rev(base::c(...))) match.arg(type)
print(formal_c()); print(formal_c("on"))
c <- function(...) paste0(base::c(...), "_g")
print(f()); print(f("quadratic_g"))
rm(c)
attach(list(c = function(...) base::c(..., "extra")), name = "masking_c", warn.conflicts = FALSE)
print(f()); print(f("ext"))
detach("masking_c")
print(f())

# A function whose closure environment binds `c`.
make <- function() {
  c <- function(...) base::c("made", ...)
  function(type = c("x", "y")) match.arg(type)
}
g <- make()
print(g()); print(g("ma"))

# Defaults that are not a plain `c()` of strings.
single <- function(type = "only") match.arg(type)
print(single()); print(single("on"))
tagged <- function(type = c(a = "alpha", b = "beta")) match.arg(type)
print(tagged()); print(tagged("be"))
with_na <- function(type = c("alpha", NA)) match.arg(type)
print(tryCatch(with_na(), error = conditionMessage))
mixed <- function(type = c("alpha", 1)) match.arg(type)
print(mixed()); print(mixed("1"))
nested <- function(type = c("alpha", c("beta", "gamma"))) match.arg(type)
print(nested()); print(nested("gam"))
empty <- function(type = c()) match.arg(type)
print(tryCatch(empty(), error = conditionMessage))

# Choices supplied explicitly, which skips the formal default.
supplied <- function(type = c("never", "used")) match.arg(type, c("left", "right"))
print(supplied("ri")); print(tryCatch(supplied(), error = conditionMessage))

# Partial matching: unique, ambiguous, exact among prefixes, and empty.
partial <- function(type = c("mean", "median", "med")) match.arg(type)
print(partial("mea")); print(partial("med")); print(tryCatch(partial("me"), error = conditionMessage))
print(tryCatch(partial(""), error = conditionMessage))

# A caller whose frame is already an environment, and callers reached in other ways.
reified <- function(type = c("north", "south")) {
  e <- environment()
  invisible(e)
  match.arg(type)
}
print(reified()); print(reified("so"))
frame_seen <- function(type = c("north", "south")) {
  force(sys.frame(sys.nframe()))
  match.arg(type)
}
print(frame_seen("no"))
print(do.call(f, list("lin")))
print(vapply(c("lin", "quad"), f, ""))
print(Reduce(function(acc, x) paste(acc, f(x)), c("quad", "lin"), ""))
inner <- function(type = c("up", "down")) identity(match.arg(type))
print(inner()); print(inner("do"))
wrapper <- function(type = c("up", "down")) (function() match.arg(type))()
print(tryCatch(wrapper(), error = conditionMessage))
outer <- function(type = c("first", "second")) inner2(type)
inner2 <- function(kind = c("third", "fourth")) match.arg(kind)
print(tryCatch(outer(), error = conditionMessage)); print(outer("fo"))
recursive <- function(type = c("deep", "shallow"), n = 2) if (n > 0) recursive(type, n - 1) else match.arg(type)
print(recursive()); print(recursive("sh"))
