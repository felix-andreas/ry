# A function that defines closures only its own variables hold: the closures and the frame's
# environment die when the call returns. Beside each is the same shape with one holder that
# outlives the call. Every function runs several times, so compiled code runs it too.
thrice <- function(f) for (i in 1:3) print(f(i))

cat("-- a helper that is called and dropped\n")
called <- function(x) {
    inner <- function() x + 1L
    inner()
}
thrice(called)
reads_and_writes <- function(x) {
    total <- 0
    add <- function(by) total <<- total + by
    add(x)
    add(x * 10)
    total
}
thrice(reads_and_writes)
two_helpers <- function(x) {
    double <- function(v) v * 2
    shift <- function(v) double(v) + x
    shift(x)
}
thrice(two_helpers)
never_called <- function(x) {
    unused <- function() stop("never runs")
    x * 3
}
thrice(never_called)
overwritten <- function(x) {
    step <- function() x
    step <- function() x + 100
    step <- step()
    step
}
thrice(overwritten)
removed <- function(x) {
    step <- function() x
    result <- step()
    rm(step)
    result
}
thrice(removed)
recursive <- function(n) {
    down <- function(k) if (k == 0L) 0L else k + down(k - 1L)
    down(n)
}
thrice(recursive)
anonymous <- function(x) (function(y) y + x)(10)
thrice(anonymous)
block <- function(x) local({
    y <- x * 2
    y + 1
})
thrice(block)

cat("-- a helper handed to a function that calls it and returns\n")
handed <- function(x) {
    scale <- function(v) v * x
    vapply(1:3, scale, numeric(1))
}
thrice(handed)
reduced <- function(x) Reduce(function(a, b) a + b * x, 1:4)
thrice(reduced)
sorted <- function(x) {
    key <- function(v) (v * 7 + x) %% 5
    order(vapply(1:6, key, numeric(1)))
}
thrice(sorted)
called_by_name <- function(x) {
    f <- function(a, b) a - b + x
    do.call(f, list(10, 1))
}
thrice(called_by_name)
guarded <- function(x) {
    tryCatch(
        if (x == 2) stop("two") else x,
        error = function(e) paste("caught", conditionMessage(e), x),
        finally = cat("finally", x, "\n")
    )
}
thrice(guarded)
with_default <- function(x, pick = function(v) v[x]) pick(c("a", "b", "c"))
thrice(with_default)
default_unread <- function(x, pick = function(v) stop("never read")) x
thrice(default_unread)

cat("-- the same shapes with a holder that outlives the call\n")
returned <- function(x) {
    inner <- function() x + 1L
    inner
}
thrice(function(i) returned(i)())
returned_after_call <- function(x) {
    inner <- function() x + 1L
    inner()
    inner
}
thrice(function(i) returned_after_call(i)())
in_a_list <- function(x) {
    inner <- function() x + 1L
    list(1, list(inner))
}
thrice(function(i) in_a_list(i)[[2]][[1]]())
as_an_attribute <- function(x) {
    inner <- function() x + 1L
    structure(1, helper = inner)
}
thrice(function(i) attr(as_an_attribute(i), "helper")())
kept <- NULL
assigned_above <- function(x) {
    inner <- function() x + 1L
    kept <<- inner
    inner()
}
thrice(function(i) c(assigned_above(i), kept()))
assigned_by_name <- function(x) {
    inner <- function() x + 2L
    assign("kept", inner, envir = globalenv())
    inner()
}
thrice(function(i) c(assigned_by_name(i), kept()))
store <- new.env()
stored <- function(x) {
    store$helper <- function() x + 3L
    0L
}
thrice(function(i) c(stored(i), store$helper()))
copied <- function(x) {
    inner <- function() x + 1L
    alias <- inner
    alias
}
thrice(function(i) copied(i)())
one_of_two <- function(x) {
    dies <- function() x
    lives <- function() dies() * 2
    dies()
    lives
}
thrice(function(i) one_of_two(i)())
environment_returned <- function(x) {
    inner <- function() x
    inner()
    environment()
}
thrice(function(i) sort(ls(environment_returned(i))))
environment_of_helper <- function(x) {
    inner <- function() x
    environment(inner)
}
thrice(function(i) get("x", environment_of_helper(i)))
formula_returned <- function(x) {
    inner <- function() x
    inner()
    y ~ x
}
thrice(function(i) get("x", environment(formula_returned(i))))
promise_returned <- function(x) {
    inner <- function() x
    delayedAssign("later", inner() + 5L)
    function() later
}
thrice(function(i) promise_returned(i)())
finalized <- function(x) {
    inner <- function() x
    reg.finalizer(environment(), function(e) NULL)
    inner()
}
thrice(finalized)
exit_handler <- function(x) {
    inner <- function() cat("leaving", x, "\n")
    on.exit(inner())
    x
}
thrice(exit_handler)
exit_keeps <- function(x) {
    inner <- function() x + 4L
    on.exit(kept <<- inner)
    x
}
thrice(function(i) c(exit_keeps(i), kept()))

cat("-- an error leaves through a frame that made a closure\n")
fails <- function(x) {
    inner <- function() stop("failed in ", x)
    inner()
}
thrice(function(i) tryCatch(fails(i), error = conditionMessage))
condition_carries <- function(x) {
    inner <- function() x
    stop(structure(class = c("carrier", "error", "condition"), list(message = "m", call = NULL, helper = inner)))
}
thrice(function(i) tryCatch(condition_carries(i), carrier = function(e) e$helper()))

cat("-- a frame whose variables became an environment before the closure was made\n")
looked_up <- function(x) {
    here <- environment()
    inner <- function() x + 1L
    c(inner(), identical(environment(inner), here))
}
thrice(looked_up)
evaluated <- function(x) {
    inner <- function() x + 1L
    eval(quote(inner())) + get("x")
}
thrice(evaluated)

cat("-- many calls, so freed frames are reused\n")
total <- 0
for (i in 1:200) total <- total + called(i) + two_helpers(i) + anonymous(i) + returned(i)()
print(total)
kept_all <- lapply(1:5, returned)
for (i in 1:50) called(i)
print(vapply(kept_all, function(f) f(), integer(1)))
invisible(gc())
print(vapply(kept_all, function(f) f(), integer(1)))
