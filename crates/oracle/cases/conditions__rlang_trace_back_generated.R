# oracle: requires rlang
# Seeded random call stacks for rlang's trace_back(): each stack nests a few frames drawn from the
# shapes condition handling produces (closures, operators, namespaced calls, do.call with inlined
# values, apply loops, handlers, eval and local, S3 dispatch), optionally trims with `top` or
# `bottom`, and compares the trace with the one the R body of trace_back() returns from the same
# frame. The digest of every trace is printed, so GNU R's run checks the values themselves.
library(rlang)

body_only <- body(rlang:::trace_back)
if (identical(body_only[[1]], as.name("{")) && length(body_only) == 3L &&
    is.call(body_only[[2]]) && identical(body_only[[2]][[1]], as.name(".Internal"))) {
    body_only <- body_only[[3]]
}
slow_trace_back <- rlang:::trace_back
body(slow_trace_back) <- body_only

# The runner may evaluate this file inside frames of its own, so traces drop the leading rows that
# are the runner's calls and renumber their parents.
harness <- sys.calls()
describe <- function(trace) {
    n <- 0L
    while (n < length(harness) && n < nrow(trace) && identical(trace$call[[n + 1L]], harness[[n + 1L]])) n <- n + 1L
    keep <- seq_len(nrow(trace)) > n
    calls <- vapply(trace$call[keep], function(call) paste(deparse(call), collapse = " "), "")
    paste(sum(keep), typeof(trace$namespace), typeof(trace$scope),
        paste(calls, pmax(trace$parent[keep] - n, 0L), trace$visible[keep], trace$namespace[keep], trace$scope[keep],
            sep = "/", collapse = ";"))
}

leaf <- function(state) {
    marks <- state$marks
    top <- switch(state$trim, top = marks[[length(marks)]], NULL)
    bottom <- switch(state$trim, bottom = marks[[min(2L, length(marks))]], NULL)
    fast <- rlang::trace_back(top = top, bottom = bottom)
    slow <- slow_trace_back(top = top, bottom = bottom)
    list(same = identical(fast, slow), text = describe(fast))
}

`%then%` <- function(state, step) step(state)
print.marker <- function(x, ...) x$run()
wrappers <- list(
    closure = function(state, next_step) next_step(state),
    operator = function(state, next_step) state %then% next_step,
    namespaced = function(state, next_step) base::identity(next_step(state)),
    do_call = function(state, next_step) do.call(function(a, b, c) next_step(state), list(1:3, mtcars[1:2, 1:2], sum)),
    lapply = function(state, next_step) lapply(1L, function(i) next_step(state))[[1L]],
    try_catch = function(state, next_step) tryCatch(next_step(state), error = function(e) NULL),
    calling = function(state, next_step) withCallingHandlers(next_step(state), warning = function(w) NULL),
    eval = function(state, next_step) eval(quote(next_step(state))),
    local = function(state, next_step) local(next_step(state)),
    dispatch = function(state, next_step) print(structure(list(run = function() next_step(state)), class = "marker")),
    mark = function(state, next_step) {
        state$marks[[length(state$marks) + 1L]] <- environment()
        next_step(state)
    }
)

run_stack <- function(kinds, trim) {
    step <- leaf
    for (kind in rev(kinds)) {
        step <- local({
            wrapper <- wrappers[[kind]]
            inner <- step
            function(state) wrapper(state, inner)
        })
    }
    step(list(marks = list(globalenv()), trim = trim))
}

set.seed(20261110)
mismatches <- 0L
digests <- character()
for (case in seq_len(150L)) {
    kinds <- sample(names(wrappers), sample(0:6, 1L), replace = TRUE)
    trim <- sample(c("none", "top", "bottom"), 1L)
    result <- run_stack(kinds, trim)
    if (!isTRUE(result$same)) {
        mismatches <- mismatches + 1L
        cat("mismatch in case", case, ":", kinds, trim, "\n")
    }
    digests[[case]] <- result$text
}
cat("mismatches:", mismatches, "\n")
cat(digests[1:5], sep = "\n")
cat("traces:", length(unique(digests)), "distinct of", length(digests), "\n")
cat("checksum:", sum(utf8ToInt(paste(digests, collapse = "\n")) %% 9973), "\n")
