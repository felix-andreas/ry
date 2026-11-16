# A site that passes `...` keeps a plan for what `...` held, and each call checks it.
show <- function(label, value) cat(label, ":", deparse(value), "\n")
attempt <- function(code) tryCatch(code, error = function(e) conditionMessage(e))

target <- function(x, y = 2L, z = x + y) c(x = x, y = y, z = z)
forward <- function(...) target(...)
around <- function(first, ...) target(first, ...)
after <- function(last, ...) target(..., z = last)

# The same arguments on every call.
for (round in 1:6) show("same", forward(round, 3L))

# The count and the names change between calls, and change back.
for (round in 1:4) {
  show("one", forward(round))
  show("two", forward(round, 5L))
  show("named", forward(round, z = 7L))
  show("other name", forward(round, y = 7L))
  show("partial name", forward(x = round, 9L))
  show("reordered", forward(y = 1L, round))
  show("three", forward(round, 5L, 6L))
}

# Arguments written beside `...`.
for (round in 1:5) {
  show("around", around(round))
  show("around two", around(round, 4L))
  show("around named", around(round, z = 0L))
  show("after", after(round, 1L))
  show("after two", after(round, 1L, 2L))
}

# Calls that do not match, between calls that do.
for (round in 1:4) {
  show("too many", attempt(forward(1L, 2L, 3L, 4L)))
  show("unknown name", attempt(forward(1L, w = 2L)))
  show("matched twice", attempt(forward(x = 1L, x = 2L)))
  show("name second", forward(1L, x = 2L))
  show("after twice", attempt(after(1L, 2L, z = 3L)))
  show("fine", forward(round, 2L))
}

# An empty `...`, and an empty argument inside it.
none <- function(a = "default", b = "other") paste(a, b)
relay <- function(...) none(...)
for (round in 1:5) {
  show("empty", relay())
  show("empty first", relay(, "given"))
  show("empty second", relay("given", ))
  show("both", relay("one", "two"))
}

# The callee collects `...` itself.
collect <- function(head, ...) list(head = head, rest = list(...), count = ...length())
pass <- function(...) collect(...)
for (round in 1:5) {
  show("collect one", pass(round))
  show("collect three", pass(round, tag = "t", round * 2))
  show("collect named head", pass(extra = 1, head = round))
}

# The callee looks at its arguments as code.
inspect <- function(x, ...) {
  list(code = substitute(x), given = !missing(x), count = nargs(), call = sys.call(),
       matched = match.call())
}
watch <- function(...) inspect(...)
for (round in 1:4) {
  show("inspect", watch(round + 1, extra = round))
  show("inspect none", watch())
}

# Promises in `...` run when the callee forces them, once.
log <- character()
note <- function(label) { log <<- c(log, label); label }
lazy <- function(a, b) { note("body"); if (nchar(b) > 0) paste(b, a) else "skipped" }
defer <- function(...) lazy(...)
for (round in 1:5) show("lazy", defer(note("a"), note("b")))
show("log", log)
unused <- function(a, b) "b is never forced"
skip <- function(...) unused(...)
for (round in 1:5) show("unforced", skip(1, stop("never runs")))

# The function at the site changes while `...` holds the same arguments.
first <- function(x, y) x - y
second <- function(y, x) x - y
third <- function(...) sum(...)
switching <- function(callee, ...) callee(...)
for (round in 1:6) {
  show("first", switching(first, 10, round))
  show("second", switching(second, 10, round))
  show("builtin", switching(third, 10, round))
}

# More shapes than a site replans for.
gather <- function(first = "none", ...) c(first, names(list(...)), ...length())
shapes <- function(...) gather(...)
for (round in 1:3) {
  show("shape a", shapes(a = 1))
  show("shape b", shapes(b = 1))
  show("shape c", shapes(c = 1))
  show("shape d", shapes(d = 1))
  show("shape e", shapes(e = 1))
  show("shape f", shapes(f = 1))
  show("shape g", shapes(g = 1))
  show("shape h", shapes(h = 1))
  show("shape i", shapes(i = 1))
  show("shape j", shapes(j = 1, 2))
  show("shape k", shapes(1, 2, 3))
}

# A frame that has an environment forwards `...` too.
keeps <- function(...) { here <- environment(); target(...) }
evaluates <- function(...) { value <- eval(quote(target(...))); value }
for (round in 1:5) {
  show("environment", keeps(round, z = 1L))
  show("eval", evaluates(round, 4L))
}

# `...` passed twice, and with `..1`.
twice <- function(...) c(...length(), length(c(..., ...)))
both <- function(...) twice(..., ...)
element <- function(...) target(..1, ...)
for (round in 1:5) {
  show("twice", both(round, 2))
  show("element", attempt(element(round)))
  show("element two", attempt(element(round, 3L)))
}

# A forwarding call that recurses.
countdown <- function(n, ...) if (n == 0L) list(...) else countdown(n - 1L, ..., n)
show("countdown", countdown(5L))
show("countdown named", countdown(3L, start = TRUE))

# Default arguments that read other arguments passed through `...`.
defaults <- function(a, b = a * 2, c = b + 1) c(a, b, c)
through <- function(...) defaults(...)
for (round in 1:5) {
  show("defaults one", through(round))
  show("defaults two", through(round, 10))
  show("defaults skip", through(round, c = 0))
}
