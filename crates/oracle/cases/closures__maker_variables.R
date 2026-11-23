# A closure made in a function reads the function's variables, and a function returns a closure
# it made. Each call runs several times, so a compiled function is covered as well as its first
# run.
noisy <- function(label, value) {
    cat("forcing", label, "\n")
    value
}

cat("-- a closure reads its maker's unforced argument\n")
reads <- function(x) {
    inner <- function() x + 1L
    c(inner(), inner())
}
for (i in 1:3) print(reads(noisy("x", i)))
reads_default <- function(x = noisy("the default", 5L)) {
    inner <- function() x
    inner() + inner()
}
for (i in 1:3) print(reads_default())
reads_later <- function(x, y) {
    inner <- function() if (x > 1L) y else "y never forced"
    inner()
}
for (i in 1:3) print(reads_later(noisy("x", i), noisy("y", "y forced")))

cat("-- a missing argument, an error and a warning in the argument\n")
reads_missing <- function(x) {
    inner <- function() x
    tryCatch(inner(), error = conditionMessage)
}
for (i in 1:3) print(reads_missing())
for (i in 1:3) print(tryCatch(reads(stop("no value")), error = conditionMessage))
for (i in 1:3) print(tryCatch(reads(undefined_variable), error = conditionMessage))
for (i in 1:3) {
    print(withCallingHandlers(reads(as.integer("a")), warning = function(w) {
        cat("warning:", conditionMessage(w), "\n")
        invokeRestart("muffleWarning")
    }))
}
retried <- function(x) {
    inner <- function() x
    first <- tryCatch(inner(), error = function(e) "failed once")
    second <- tryCatch(inner(), error = function(e) "failed twice")
    c(first, second)
}
attempts <- 0L
for (i in 1:3) print(retried({
    attempts <<- attempts + 1L
    if (attempts %% 2L == 1L) stop("odd attempt") else "even attempt"
}))
own_default <- function(x = x) {
    inner <- function() x
    tryCatch(inner(), error = function(e) "a default that reads itself")
}
for (i in 1:3) print(own_default())

cat("-- the argument is evaluated where the call was written\n")
forced_inside <- function(x) {
    inner <- function() x
    inner()
}
for (i in 1:3) print(forced_inside(sys.nframe()))
for (i in 1:3) print(forced_inside(environmentName(environment())))
from_function <- function() {
    local_value <- "the caller's local"
    forced_inside(local_value)
}
for (i in 1:3) print(from_function())
for (i in 1:3) print(forced_inside(deparse(sys.call())))

cat("-- nested closures and functionals\n")
nested <- function(x) function() function() x
for (i in 1:3) print(nested(noisy("nested", i))()())
scaled <- function(x) vapply(1:3, function(i) x * i, 0)
for (i in 1:3) print(scaled(noisy("scaled", i)))
twice <- function(x) {
    first <- function() x
    second <- function() first() + x
    second()
}
for (i in 1:3) print(twice(noisy("twice", i)))
assigned <- function(x) {
    inner <- function() x
    before <- inner()
    x <- "assigned after the read"
    c(before, inner())
}
for (i in 1:3) print(assigned(noisy("assigned", "the argument")))
superassigned <- function(x) {
    inner <- function() {
        x <<- x + 1L
        x
    }
    c(inner(), inner(), x)
}
for (i in 1:3) print(superassigned(noisy("superassigned", i)))
substituted <- function(x) {
    inner <- function() x
    list(deparse(substitute(x)), inner(), deparse(substitute(x)))
}
for (i in 1:3) str(substituted(noisy("substituted", i)))

cat("-- a function returns a closure\n")
maker <- function(x) function() x
for (i in 1:3) {
    made <- maker(noisy("made", i))
    print(c(made(), made()))
}
for (i in 1:3) print(ls(environment(maker(i))))
make_counter <- function() {
    count <- 0L
    function() {
        count <<- count + 1L
        count
    }
}
for (i in 1:3) {
    counter <- make_counter()
    print(c(counter(), counter(), counter()))
}
adders <- lapply(1:3, function(n) function(x) x + n)
print(vapply(adders, function(add) add(10L), 0L))
accessors <- function(x) list(get = function() x, set = function(value) x <<- value)
for (i in 1:3) {
    pair <- accessors(i)
    pair$set(pair$get() * 10L)
    print(pair$get())
}
explicit <- function(x) {
    if (x > 1L) return(function() "the early closure")
    function() "the last value"
}
for (i in 1:3) print(explicit(i)())
leaving <- function(x) {
    on.exit(cat("leaving the maker\n"))
    function() x
}
for (i in 1:3) print(leaving(i)())
quiet <- function(x) invisible(function() x)
for (i in 1:3) print(withVisible(quiet(i))$visible)
kept <- NULL
keeps <- function(x) {
    kept <<- function() x
    0L
}
for (i in 1:3) {
    keeps(noisy("kept", i))
    print(kept())
}
handled <- function(x) tryCatch(function() x, error = identity)
for (i in 1:3) print(handled(noisy("handled", i))())
in_list <- function(x) {
    local_value <- x * 2L
    list(function() local_value, function() x)
}
for (i in 1:3) print(vapply(in_list(i), function(f) f(), 0L))
never_called <- function(x) {
    unused <- function() x
    "the closure is dropped"
}
for (i in 1:3) print(never_called(noisy("never forced", i)))
recursive_maker <- function(n) if (n == 0L) function() "the innermost" else recursive_maker(n - 1L)
for (i in 1:3) print(recursive_maker(i)())
