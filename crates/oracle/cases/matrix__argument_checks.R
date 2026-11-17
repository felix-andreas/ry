# `matrix()` checks its data and extents in GNU R's order and warns when the data does not fill
# the matrix evenly.
t <- function(e) { r <- withCallingHandlers(tryCatch(e, error = function(e) paste("error:", conditionMessage(e), "|", deparse(conditionCall(e))[1])), warning = function(w) { cat("warning:", conditionMessage(w), "|", deparse(conditionCall(w))[1], "\n"); invokeRestart("muffleWarning") }); if (is.character(r) && length(r) == 1 && startsWith(r, "error:")) cat(r, "\n") else cat(deparse(dim(r)), ":", deparse(as.vector(r))[1], "\n") }
for (n in list(0, 1, 2, 3, 4, 5, 7)) for (d in list(1:6, 1:7, 1:2, 1, integer(0), 1:12)) {
  cat("n", n, "len", length(d), "\n")
  t(matrix(d, n)); t(matrix(d, ncol = n)); t(matrix(d, n, 2)); t(matrix(d, 3, n)); t(matrix(d, n, n, byrow = TRUE))
}
t(matrix(pi, -1, -1)); t(matrix(1:6, 2, NA)); t(matrix(1:6, NA)); t(matrix(1:6, "a")); t(matrix(1:6, 2, "a")); t(matrix(c())); t(matrix(NULL, 2, 2));
t(matrix(1:6, 2.9)); t(matrix(1:6, TRUE)); t(matrix(1:6, c(2, 3))); t(matrix(1:6, integer(0))); t(matrix(1:6, 2, byrow = NA)); t(matrix(1:6, 2, byrow = "a")); t(matrix(1:6, 2, byrow = c(TRUE, FALSE)))
t(matrix(1:6, 2, dimnames = 1)); t(matrix(1:6, 2, dimnames = list("a"))); t(matrix(1:6, 2, dimnames = list(c("a", "b"), NULL, NULL))); t(matrix(1:6, 2, dimnames = list(NULL)))
t(matrix(list(1, "a"), 2, 2)); t(matrix(expression(a, b), 1)); t(matrix(1:6, 1e10)); t(matrix(1:6, ncol = -2)); t(matrix(1:6, NULL)); t(matrix(1:6, factor("a")))
t(matrix(as.raw(1:3), 2, 2)); t(matrix(c(a = 1, b = 2))); t(matrix(1:3, 0)); t(matrix(1:3, ncol = 0)); t(matrix(integer(0), 0)); t(matrix(1, 0, 3)); t(matrix(1:2, 0, 3))
