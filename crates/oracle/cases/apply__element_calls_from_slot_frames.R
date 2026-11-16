# lapply and vapply call FUN(X[[i]], ...) in their own frame. Each shape runs three times
# inside a function, so the later runs use compiled code.
thrice <- function(f) for (k in 1:3) print(f())

# plain closures, with and without extra arguments
thrice(function() lapply(1:3, function(x) x * 2L))
thrice(function() vapply(c(a = 1, b = 2), function(x) x + 0.5, numeric(1)))
thrice(function() sapply(list(1:2, 3:6), length))
thrice(function() lapply(list(1, "a", NULL, TRUE), function(x) class(x)))
thrice(function() lapply(1:2, function(x, y, z) x + y * z, 10, z = 2))
thrice(function() vapply(letters[1:3], function(s, suffix) paste0(s, suffix), "", suffix = "!"))
thrice(function() unlist(lapply(c(1.5, 2.5), function(x, ...) sum(x, ...), 1, 2)))

# what FUN sees of its call and of the frame it was called from
thrice(function() lapply(1:2, function(x) sys.call()))
thrice(function() lapply(1:2, function(x) match.call()))
thrice(function() lapply(1:2, function(x) deparse(substitute(x))))
thrice(function() lapply(1:2, function(x, y) c(missing(y), nargs())))
thrice(function() lapply(4:5, function(x) parent.frame()$i[]))
thrice(function() lapply(4:5, function(x) parent.frame()$i))
thrice(function() lapply(4:5, function(x) eval(quote(i), parent.frame())))
thrice(function() lapply(1:2, function(x) sort(ls(parent.frame(), all.names = TRUE))))
thrice(function() lapply(1:2, function(x) identical(parent.frame(), sys.frame(sys.parent()))))
thrice(function() lapply(1:2, function(x) environmentName(environment(sys.function(sys.parent())))))
thrice(function() vapply(1:3, function(x) parent.frame()$i[] * 10L, integer(1)))
thrice(function() lapply(1:2, function(x) sys.nframe() - sys.parent()))
thrice(function() vapply(7:8, function(x) get("i", envir = parent.frame()) + x, numeric(1)))

# FUN changes the frame it was called from
thrice(function() lapply(1:3, function(x) { assign("i", 99L, parent.frame()); x }))
thrice(function() lapply(1:3, function(x) {
  if (x == 1L) assign("FUN", function(x) -x, parent.frame())
  x
}))
thrice(function() lapply(1:3, function(x) { rm("i", envir = parent.frame()); x }))
thrice(function() {
  kept <- list()
  lapply(1:3, function(x) kept[[x]] <<- parent.frame())
  vapply(kept, function(e) e$i, 1L)
})

# the first argument is forced, the others stay lazy and are forced once
thrice(function() {
  fs <- lapply(1:3, function(k) function() k)
  c(fs[[1]](), fs[[3]]())
})
thrice(function() lapply(1:2, function(x, unused) x, stop("never forced")))
thrice(function() lapply(1:3, function(x, y) y, { cat("forced once\n"); 5 }))
thrice(function() lapply(1:2, function(x, y) deparse(substitute(y)), a + b))
thrice(function() {
  local_value <- 7
  lapply(1:2, function(x, y) x + y, local_value)
})
thrice(function() {
  outer <- function(...) lapply(1:2, function(x, ...) list(...), ...)
  outer(p = 1, "q")
})

# errors, warnings and conditions name the element call
thrice(function() tryCatch(lapply(1:2, function(x) stop("bad ", x)), error = function(e) conditionCall(e)))
thrice(function() withCallingHandlers(
  lapply(1:2, function(x) { warning("careful"); x }),
  warning = function(w) { print(conditionCall(w)); invokeRestart("muffleWarning") }
))
thrice(function() tryCatch(vapply(1:2, function(x) "a", numeric(1)), error = function(e) conditionMessage(e)))
thrice(function() tryCatch(lapply(1:2, function(x, y) y), error = function(e) conditionMessage(e)))

# shapes that read X[[i]] through the frame
thrice(function() lapply(as.list(quote(f(a, ))), function(x) missing(x)))
thrice(function() lapply(structure(list(1, 2), class = "rec"), function(x) x + 1))
thrice(function() {
  `[[.rec` <- function(x, i) unclass(x)[[i]] * 100
  vapply(structure(list(1, 2), class = "rec"), function(x) x, 1)
})
thrice(function() lapply(factor(c("u", "v")), function(x) as.character(x)))
thrice(function() lapply(1:2, "-", 1L))
thrice(function() vapply(list(1:3, 4:9), length, 1L))
thrice(function() lapply(list(a = 1, b = "x"), is.numeric))

# nesting and recursion
thrice(function() lapply(1:2, function(x) vapply(1:x, function(y) y * x, 1)))
thrice(function() {
  depth <- function(node) if (is.list(node)) 1L + max(0L, vapply(node, depth, 1L)) else 0L
  depth(list(1, list(2, list(3)), 4))
})
thrice(function() Map(function(a, b) a + b, 1:2, 3:4))
thrice(function() Filter(function(x) x > 1, 1:3))
thrice(function() Reduce(`+`, lapply(1:3, function(x) x), accumulate = TRUE))
thrice(function() do.call(lapply, list(1:2, function(x) x + 1L)))
thrice(function() lapply(X = 1:2, FUN = function(x) parent.frame()$X))

# a builtin FUN, with extra arguments, tags, objects, errors and warnings
length.box <- function(x) 99L
thrice(function() vapply(c(1.26, 2.71), round, 1, digits = 1))
thrice(function() lapply(list(1:3, letters), length))
thrice(function() lapply(list(structure(list(), class = "box"), 1:2), length))
thrice(function() lapply(list(1, factor("a"), Sys.Date()), is.numeric))
thrice(function() vapply(list(NULL, 1, NULL), is.null, NA))
thrice(function() lapply(list(a = 1:2, b = 3), sum, 10, na.rm = TRUE))
thrice(function() { n <- 0; r <- lapply(1:3, sum, { n <- n + 1; 100 }); c(unlist(r), n) })
thrice(function() lapply(1:2, sum, length(sys.frames())))
thrice(function() tryCatch(lapply(list(1, "a"), sqrt), error = function(e) deparse(conditionCall(e))))
thrice(function() tryCatch(lapply(1:2, sum, stop("lazy")), error = function(e) conditionMessage(e)))
thrice(function() withCallingHandlers(
  lapply(c(-1, 4), sqrt),
  warning = function(w) { print(conditionCall(w)); invokeRestart("muffleWarning") }
))
thrice(function() tryCatch(lapply(1:2, c, , 3), error = function(e) conditionMessage(e)))
thrice(function() lapply(list(1:2, 3:4), `names<-`, c("p", "q")))
thrice(function() { l <- list(c(a = 1), c(b = 2)); r <- lapply(l, `names<-`, "z"); list(l, r) })
thrice(function() lapply(list(quote(f(x)), quote(g(y, z))), as.list))
thrice(function() vapply(list(1:3, 4:6), `[[`, 0L, 2L))
thrice(function() lapply(1:2, environment))
thrice(function() vapply(1:2, function(x) x, 1L, USE.NAMES = FALSE))
