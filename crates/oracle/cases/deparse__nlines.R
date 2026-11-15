# deparse(nlines = ) stops after that many lines. Nothing past the cut is deparsed, so a value
# there that would make the deparse incomplete raises no warning.
show <- function(x) print(x)
long <- seq(0.5, 150, by = 0.5)
show(deparse(long, nlines = 1L))
show(deparse(long, nlines = 2L))
show(deparse(long, nlines = 2))
show(deparse(long, width.cutoff = 500L, nlines = 1L))
show(deparse(long, width.cutoff = 20L, nlines = 3L))
show(length(deparse(long)))
show(length(deparse(long, nlines = 1000L)))
show(length(deparse(long, nlines = 0L)))
show(length(deparse(long, nlines = -1L)))
show(length(deparse(long, nlines = NA)))
show(deparse(1:3, nlines = 1L))
show(deparse(1:3, nlines = 5L))
show(deparse(character(0), nlines = 1L))
show(deparse(NULL, nlines = 1L))

nested <- list(a = long, b = list(c = letters, d = quote(f(x, y))), e = "end")
show(deparse(nested, nlines = 1L))
show(deparse(nested, nlines = 3L))
show(deparse(nested, nlines = 1L, control = c("keepNA", "keepInteger", "niceNames", "showAttributes")))
show(deparse(nested, nlines = 2L, control = NULL))
show(deparse(list(letters, LETTERS, long), nlines = 2L))
show(deparse(c(paste(rep("word", 30), collapse = " "), "b", "c"), nlines = 1L))
show(deparse(c(a = 1.5, b = 2.5, long), nlines = 2L))
show(deparse(as.integer(long * 7), nlines = 1L))
show(deparse(long > 70, nlines = 1L))
show(deparse(complex(real = long, imaginary = 1), nlines = 1L))

classed <- structure(long, class = "series", index = rev(long), dim = c(150L, 2L))
show(deparse(classed, nlines = 1L))
show(deparse(classed, nlines = 2L))
show(deparse(matrix(1:200 / 7, 20), nlines = 1L))
frame <- data.frame(a = 1:50, b = (1:50) / 3, c = rep(c("u", "v"), 25))
show(deparse(frame, nlines = 1L))
show(deparse(frame, nlines = 2L))
show(deparse(factor(rep(letters, 4)), nlines = 1L))

fn <- function(x, y) {
    z <- x + y
    if (z > 1) "big" else "small"
}
show(deparse(fn, nlines = 1L))
show(deparse(fn, nlines = 3L))
show(deparse(quote(if (a) { b; c } else { d }), nlines = 2L))
show(deparse(quote(function(x) { x; for (i in x) print(i) }), nlines = 2L))
show(deparse(body(fn), nlines = 2L))
show(deparse(quote(f(a_long_argument_name = 1, another_long_argument_name = 2, a_third_long_argument_name = 3, a_fourth = 4)), nlines = 1L))

# An environment cannot be deparsed to source, which warns under "warnIncomplete", but only when
# the deparse reaches it.
show(deparse(list(long, globalenv()), control = "all", nlines = 1L))
show(deparse(list(globalenv(), long), control = "all", nlines = 1L))
show(length(deparse(list(long, globalenv()), control = "all", nlines = 1000L)))
show(deparse(list(1, globalenv()), control = "all", nlines = 1L))

# The callers that use it: a label for an unnamed argument.
label <- function(x) deparse(substitute(x), nlines = 1L)
show(label(c(first_value, second_value)))
show(do.call(label, list(long)))
show(do.call(label, list(nested)))
show(deparse1(long, nlines = 1L))
show(nchar(deparse1(long)))
