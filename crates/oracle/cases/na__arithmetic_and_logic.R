NA + 1
NA_integer_ * 0L
NA & FALSE
NA | TRUE
NA && FALSE
isTRUE(NA)
NA > 1
NA^0
NULL > 1
sum(c(1, NA, 3))
sum(c(1, NA, 3), na.rm = TRUE)
mean(c(NA, 1, 2), na.rm = TRUE)
is.na(c(1, NA, NaN, Inf))
is.nan(c(1, NA, NaN, Inf))
NA_character_ == "a"
which(c(TRUE, NA, FALSE, TRUE))
any(c(FALSE, NA))
all(c(TRUE, NA))
any(c(TRUE, NA))
max(c(1, NA, 3))
range(c(5, NA, 2), na.rm = TRUE)
tryCatch(if (NA) 1, error = function(e) conditionMessage(e))
