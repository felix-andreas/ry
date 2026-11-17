# The bitwise functions take integer-valued operands of one type; a double is truncated to an
# integer and a shift count of any type is coerced.
t <- function(e) { r <- withCallingHandlers(tryCatch(e, error = function(e) paste("error:", conditionMessage(e), "|", deparse(conditionCall(e))[1])), warning = function(w) { cat("warning:", conditionMessage(w), "|", deparse(conditionCall(w))[1], "\n"); invokeRestart("muffleWarning") }); cat(deparse(r)[1], "\n") }
t(bitwAnd(1:3, 1:2)); t(bitwAnd(1:4, 1:2)); t(bitwAnd(integer(0), 1:2)); t(bitwAnd(1L, integer(0))); t(bitwAnd(1, integer(0))); t(bitwAnd(NULL, NULL)); t(bitwAnd(NULL, 1L)); t(bitwAnd(1L, NULL))
t(bitwAnd(1i, 1i)); t(bitwAnd(list(1), list(1))); t(bitwAnd(as.raw(1), as.raw(1))); t(bitwAnd(TRUE, 1L)); t(bitwAnd(1L, "a")); t(bitwAnd(2^40, 1)); t(bitwAnd(NaN, 1)); t(bitwAnd(structure(1L, class = "foo"), 1L)); t(bitwAnd(c(a = 3L), c(b = 1L)))
t(bitwNot(integer(0))); t(bitwNot(NA)); t(bitwNot(1:3)); t(bitwNot(c(a = 1L))); t(bitwNot(2^31)); t(bitwNot(NA_real_)); t(bitwNot(as.raw(1))); t(bitwNot(1i))
t(bitwShiftL(1L, NA)); t(bitwShiftL(NA, 1L)); t(bitwShiftL(1L, 31L)); t(bitwShiftL(-1L, 1L)); t(bitwShiftL(1:3, 1:2)); t(bitwShiftL(1L, 1.9)); t(bitwShiftL(1L, "1")); t(bitwShiftL(1L, TRUE)); t(bitwShiftL(1, NULL)); t(bitwShiftR(1.5, 1)); t(bitwShiftR(-8, 1)); t(bitwShiftR(-8L, 33L)); t(bitwShiftL(integer(0), 1L)); t(bitwShiftL(1L, integer(0))); t(bitwShiftL(2^31, 1)); t(bitwShiftL(1i, 1)); t(bitwShiftL(TRUE, 1L)); t(bitwShiftL(1L, list(1)))
t(bitwAnd(1, 2)); t(bitwAnd(5, 3)); t(bitwAnd(1.5, 1)); t(bitwAnd(1L, 1)); t(bitwAnd(TRUE, TRUE)); t(bitwAnd("a", "b")); t(bitwAnd(1:3, 1L)); t(bitwAnd(integer(0), 1L)); t(bitwAnd(NA_integer_, 1L))
t(bitwNot(1)); t(bitwNot(1.5)); t(bitwNot(TRUE)); t(bitwNot(NULL)); t(bitwNot("a"))
t(bitwOr(1, 2)); t(bitwXor(1, 3)); t(bitwXor(1L, 3)); t(bitwOr(2^31, 1)); t(bitwAnd(-1, 1)); t(bitwAnd(NA_real_, 1))
t(bitwShiftL(1, 2)); t(bitwShiftL(1L, 2L)); t(bitwShiftL(1.5, 2)); t(bitwShiftR(8, 1)); t(bitwShiftR(-1, 1)); t(bitwShiftL(1L, 32L)); t(bitwShiftL(1L, -1L)); t(bitwShiftL("a", 1)); t(bitwShiftL(1L, 1:3)); t(bitwShiftL(NULL, 1L)); t(bitwShiftR(TRUE, 1L))
