# A decimal mark that is not one column wide warns each time a double is written with it: once
# per double (NA included), once per part of a non-NA complex, and never for an integer. A
# one-column mark, ASCII or not, writes silently.
collect <- function(expr) {
    seen <- character()
    value <- withCallingHandlers(expr, warning = function(w) {
        seen <<- c(seen, paste(deparse(conditionCall(w))[[1L]], "|", conditionMessage(w)))
        invokeRestart("muffleWarning")
    })
    list(value = value, warnings = seen)
}
show <- function(result) {
    print(result$value)
    for (w in unique(result$warnings)) cat(sum(result$warnings == w), "x", w, "\n")
}
show(collect(format(c(1.5, 2.5, 3), decimal.mark = "")))
show(collect(format(c(1.5, NA), decimal.mark = "ab")))
show(collect(format(c(1L, NA), decimal.mark = "ab")))
show(collect(format(c(1 + 2.5i, NA), decimal.mark = "ab")))
show(collect(format(1.5, decimal.mark = "·")))
show(collect(format(0.25, nsmall = 3, decimal.mark = ",")))
old <- options(OutDec = ",")
print(c(1.5, NA)); cat(2.5, "\n")
options(old)
# GNU R's own output under a two-column OutDec reuses a static buffer and garbles the numbers,
# so only the warnings are compared there.
z <- c(1 + 1i, NA)
suppressWarnings(old <- options(OutDec = "ab"))
printed <- collect(capture.output(print(c(1.5, NA))))$warnings
written <- collect(capture.output(cat(2.5, NA_real_, z, 3L, "\n")))$warnings
integers <- collect(capture.output(print(1:2)))$warnings
options(old)
print(table(printed)); print(table(written)); print(length(integers))
