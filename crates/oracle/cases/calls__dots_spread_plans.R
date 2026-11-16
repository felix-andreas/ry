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

# A name in `...` matches a formal partially, and a formal after `...` only exactly.
partial <- function(value, ..., verbose = FALSE, width = 1L) {
  list(value = value, rest = list(...), verbose = verbose, width = width)
}
abbreviate <- function(...) partial(...)
for (round in 1:5) {
  show("partial", abbreviate(val = round, 2L))
  show("after dots", abbreviate(round, verb = TRUE, width = 3L))
  show("exact after dots", abbreviate(round, verbose = TRUE, wid = 3L))
}

# The calling frame leaves its slots before the call, and between two calls of one site.
looks <- function(...) { first <- 1L; get("first"); target(...) }
assigns <- function(...) { assign("made", 2L); target(..., y = made) }
leaves <- function(...) {
  result <- integer()
  for (round in 1:4) {
    result <- c(result, target(...))
    if (round == 2L) environment()
  }
  result
}
for (round in 1:4) {
  show("looks", looks(round, 3L))
  show("assigns", assigns(round))
  show("leaves", leaves(round, z = 0L))
}

# `...` that is no argument list, and the `...` of an enclosing function.
assigned <- function() { ... <- 5; attempt(target(...)) }
count <- function(...) ...length()
enclosing <- function(...) { inner <- function() count(...); inner() }
unset <- function(...) { inner <- function() { if (FALSE) ... <- 1; count(...) }; inner() }
unset_builtin <- function(...) { inner <- function() { if (FALSE) ... <- 1; c(...) }; inner() }
for (round in 1:5) {
  show("assigned", assigned())
  show("enclosing", enclosing(round, 2))
  show("unset", unset(round, 2))
  show("unset builtin", unset_builtin(round, 2))
}

# An argument beside `...` whose value is the empty argument.
choose <- function(a = 5, ...) if (missing(a)) "default" else "given"
forced <- function(x, ...) { force(x); choose(x, ...) }
for (round in 1:5) show("given", forced(round, 2))
show("empty value", attempt(forced(quote(expr = ), 2)))
for (round in 1:3) show("given again", forced(round, 2))
unforced <- function(x, ...) choose(x, ...)
for (round in 1:5) show("variable", unforced(round, 2))
for (round in 1:3) show("missing variable", unforced())
missing_through <- function(x) forward(x)
for (round in 1:3) show("missing forwarded", attempt(missing_through()))

# A forwarding call inside an argument and inside a default.
inside <- function(...) identity(target(...))
default <- function(..., value = target(...)) value
for (round in 1:5) {
  show("inside", inside(round, 6L))
  show("default", default(round, y = 0L))
}

# The function changes after the site stopped replanning, and its formals change.
callees <- list(
  function(a, b) c(a, b), function(b, a) c(a, b), function(a, ...) c(a, ...length()),
  function(..., a) c(a, ...length()), function(a, b = 0) a + b, function(b = 0, a) a - b,
  function(x, a) c(x, a), function(a, x) c(x, a), function(a, bb) a * bb,
  function(a, b) a / b
)
many <- function(callee, ...) callee(...)
for (round in 1:2) {
  for (callee in callees) show("callee", many(callee, a = 8, 2))
}
changing <- function(x, y = 1) c(x, y)
relays <- function(...) changing(...)
for (round in 1:4) show("before", relays(round))
formals(changing) <- alist(y = 1, x = )
for (round in 1:4) show("after", relays(round))
formals(changing)$y <- 9
for (round in 1:4) show("default changed", relays(x = round))

# `do.call` builds the arguments.
for (round in 1:4) {
  show("do.call", do.call(forward, list(round, y = 2L)))
  show("do.call names", do.call(forward, list(z = 0L, x = round)))
}

# Two forwarding functions into a generic, its method and the next method.
describe <- function(x, ...) UseMethod("describe")
describe.default <- function(x, digits = 1L, ...) c(kind = "default", digits = digits, rest = ...length())
describe.special <- function(x, ..., digits = 4L) c(kind = "special", NextMethod(digits = digits))
outer_wrapper <- function(...) inner_wrapper(...)
inner_wrapper <- function(...) describe(...)
special <- structure(1, class = "special")
for (round in 1:5) {
  show("plain", outer_wrapper(round, 2L))
  show("special", outer_wrapper(special, round))
  show("special named", outer_wrapper(special, digits = round, extra = 0))
}
