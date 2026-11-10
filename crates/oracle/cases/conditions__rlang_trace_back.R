# oracle: requires rlang
# rlang's trace_back() in the shapes testthat and rlang's own condition handling build: nested
# closures, namespaced and inlined calls, operators, trimming with `top` and with
# `rlang_trace_top_env`, `bottom` frames, and handler frames. Each trace is also compared with
# what the R body of trace_back() returns from the same frame.
library(rlang)

# The runner may evaluate this file inside frames of its own, so traces drop the leading rows that
# are the runner's calls and renumber their parents.
harness <- sys.calls()
strip_harness <- function(trace) {
    n <- 0L
    while (n < length(harness) && n < nrow(trace) && identical(trace$call[[n + 1L]], harness[[n + 1L]])) n <- n + 1L
    keep <- seq_len(nrow(trace)) > n
    columns <- lapply(unclass(trace), function(values) values[keep])
    columns$parent <- pmax(columns$parent - n, 0L)
    columns
}

show <- function(trace) {
    cat("attributes:", names(attributes(trace)), "\n")
    cat("class:", class(trace), " version:", attr(trace, "version"), "\n")
    columns <- strip_harness(trace)
    cat("rows:", length(columns$call), "\n")
    for (column in names(columns)) {
        values <- columns[[column]]
        if (is.list(values)) values <- vapply(values, function(call) paste(deparse(call), collapse = " "), "")
        cat(column, " <", typeof(columns[[column]]), ">: ", paste(values, collapse = " | "), "\n", sep = "")
    }
    invisible(trace)
}

body_only <- body(rlang:::trace_back)
if (identical(body_only[[1]], as.name("{")) && length(body_only) == 3L &&
    is.call(body_only[[2]]) && identical(body_only[[2]][[1]], as.name(".Internal"))) {
    body_only <- body_only[[3]]
}
slow_trace_back <- rlang:::trace_back
body(slow_trace_back) <- body_only
both <- function(top = NULL, bottom = NULL) {
    fast <- rlang::trace_back(top, bottom)
    slow <- slow_trace_back(top, bottom)
    cat("same as the R body:", identical(fast, slow), "\n")
    fast
}

f <- function() g()
g <- function() h(1)
h <- function(x) both()
show(f())
from_global <- f()
show(from_global)

show(base::identity(local(both())))
show(eval(quote(both())))
show((function() both())())
show(do.call(function(x, y, z) both(), list(1:3, data.frame(a = 1), sum)))
show(lapply(1, function(i) both())[[1]])
`%op%` <- function(a, b) both()
show(1 %op% 2)
show(Reduce(function(a, b) both(), 1:2))
show(tryCatch(stop("x"), error = function(e) both()))
invisible(withCallingHandlers(
    warning("w"),
    warning = function(w) {
        show(both())
        invokeRestart("muffleWarning")
    }
))

k <- function() {
    e <- environment()
    m <- function() n()
    n <- function() both(top = e)
    m()
}
show(k())

outer <- function() {
    old <- options(rlang_trace_top_env = environment())
    on.exit(options(old))
    inner()
}
inner <- function() both()
show(outer())

b1 <- function() {
    e <- environment()
    b2(e)
}
b2 <- function(e) b3(e)
b3 <- function(e) both(bottom = e)
show(b1())
show(both(bottom = globalenv()))

entraced <- function() {
    withCallingHandlers(
        stop("boom"),
        error = function(cnd) {
            cnd <- rlang::cnd_entrace(cnd)
            show(cnd$trace)
            invokeRestart("abort_here")
        }
    )
}
withRestarts(entraced(), abort_here = function() cat("aborted\n"))
