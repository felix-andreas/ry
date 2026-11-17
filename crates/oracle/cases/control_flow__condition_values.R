# What `if`, `while` and `repeat` make of each kind of condition value: only a logical NA is a
# "missing value", every other value without a truth value is "not interpretable as logical".
show <- function(x) tryCatch(x, error = function(e) cat("E:", conditionMessage(e), "|", deparse(conditionCall(e))[1], "\n"))
one <- new.env()
assign("a", 1, one)
two <- new.env()
assign("a", 1, two)
assign("b", 1, two)
values <- list(NA, NA_integer_, NA_real_, NaN, NA_complex_, NA_character_, "yes", "TRUE", "T", "true", "false", "F",
    " TRUE", list(1), list(TRUE), list(), quote(x), quote(f(x)), quote(f(x, y)), expression(TRUE), expression(), sum,
    function(x) x, new.env(), one, two, as.raw(0), as.raw(1), 0+0i, 0+1i, complex(real = NaN, imaginary = 1), NULL,
    logical(0), c(TRUE, FALSE), 1:2, factor("a"), Sys.Date(), structure(TRUE, class = "foo"), 2L, -0.5, 1e-320,
    .Machine$integer.max, pairlist(TRUE), pairlist(1, 2), as.name("TRUE"), globalenv())
for (i in seq_along(values)) {
    value <- values[[i]]
    cat(i, "")
    result <- show(if (value) "yes" else "no")
    if (is.character(result)) cat(result, "\n")
}
for (i in seq_along(values)) {
    value <- values[[i]]
    cat(i, "")
    result <- show({
        count <- 0
        while (value) {
            count <- count + 1
            if (count > 1) break
        }
        count
    })
    if (is.numeric(result)) cat(result, "\n")
}
compiled <- compiler::cmpfun(function(value) if (value) "yes" else "no")
for (i in seq_along(values)) {
    cat(i, "")
    result <- show(compiled(values[[i]]))
    if (is.character(result)) cat(result, "\n")
}
# The same closure, called until it is compiled.
looping <- function(value) {
    count <- 0
    repeat {
        count <- count + 1
        if (value) break
        if (count > 2) break
    }
    count
}
for (round in 1:3) for (i in c(2, 7, 14, 17)) {
    cat(i, "")
    result <- show(looping(values[[i]]))
    if (is.numeric(result)) cat(result, "\n")
}
# Other readers of a truth value.
show(NA_integer_ && TRUE)
show(TRUE && "x")
show(list(1) || TRUE)
show(FALSE || quote(x))
print(show(NA_real_ || FALSE))
print(show("TRUE" && TRUE))
show(1i && TRUE)
show(stopifnot(NA_integer_))
show(stopifnot("x"))
show(vapply(1, function(i) if (NA_real_) 1 else 2, 0))
show(Filter(function(x) "a", 1:3))
print(show(Filter(function(x) NA_integer_, 1:3)))
show(while (list()) 1)
show(if (character(0)) 1)
