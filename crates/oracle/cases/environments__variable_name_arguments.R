# The variable lookups call a bad name "invalid first argument", while `assign` and `getOption`
# refuse an empty name as a zero-length variable name.
show <- function(expr) tryCatch(
    print(expr),
    error = function(e) cat("error:", conditionMessage(e), "|", deparse(conditionCall(e)), "\n")
)
show(get(1))
show(get(""))
show(get(character()))
show(get0(1))
show(get0(""))
show(exists(1))
show(exists(""))
show(assign(1, 2))
show(assign("", 2))
show(assign(character(), 2))
show(getOption("", 1))
show(getOption(1, 1))
show(getOption(c("a", "b")))
show(getOption(NA_character_, 3))
