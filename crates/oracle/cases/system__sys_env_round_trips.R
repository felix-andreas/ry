# Round trips through Sys.setenv, Sys.unsetenv and Sys.getenv: setting a variable to the value it
# already holds, unsetting one that is absent, a name repeated in one call, names that are prefixes
# of one another, values that hold `=`, empty against unset, and the save-and-restore pattern
# withr's local_envvar() follows.
show <- function(expr) {
    value <- tryCatch(expr, error = function(e) paste("error:", conditionMessage(e)))
    str(value)
}
names <- c("RY_RT", "RY_RT_A", "RY_RTA", "RY_R")
invisible(Sys.unsetenv(names))
show(Sys.getenv(names, unset = NA))
show(Sys.setenv(RY_RT = "x", RY_RT_A = "y"))
show(Sys.getenv(names, unset = NA))
show(Sys.setenv(RY_RT = "x"))
show(Sys.getenv("RY_RT"))
show(Sys.setenv(RY_RT = "first", RY_RT = "second", RY_RT = "first"))
show(Sys.getenv("RY_RT"))
show(Sys.setenv(RY_RTA = "B=x"))
show(Sys.getenv(c("RY_RTA", "RY_RTA=B", "RY_RT", "RY_R"), unset = "<none>"))
show(Sys.setenv(RY_RT_A = ""))
show(Sys.getenv("RY_RT_A", unset = NA))
show(nchar(Sys.getenv("RY_RT_A", unset = NA)))
show(Sys.unsetenv(c("RY_RT_A", "RY_RT_A", "RY_NEVER_SET")))
show(Sys.getenv(c("RY_RT_A", "RY_NEVER_SET"), unset = NA))
show(Sys.unsetenv("RY_RT_A"))
show(Sys.getenv(c("RY_RT", "RY_RT", "RY_RT_A", "RY_RT"), unset = "-", names = TRUE))
long <- strrep("v", 500)
show(Sys.setenv(RY_RT = long))
show(identical(Sys.getenv("RY_RT"), long))
show(Sys.setenv(RY_RT = "été"))
show(Sys.getenv("RY_RT") == "été")
# Save the current values, change them, then restore: set what was set, unset what was not.
save_and_change <- function(new) {
    old <- Sys.getenv(names(new), unset = NA, names = TRUE)
    is_set <- !is.na(new)
    if (any(is_set)) do.call(Sys.setenv, as.list(new[is_set]))
    if (any(!is_set)) Sys.unsetenv(names(new)[!is_set])
    old
}
restore <- function(old) {
    is_set <- !is.na(old)
    if (any(is_set)) do.call(Sys.setenv, as.list(old[is_set]))
    if (any(!is_set)) Sys.unsetenv(names(old)[!is_set])
    invisible()
}
before <- Sys.getenv(names, unset = NA, names = TRUE)
for (round in 1:3) {
    old <- save_and_change(c(RY_RT = paste0("r", round), RY_RT_A = NA, RY_RTA = "", RY_R = "z"))
    show(Sys.getenv(names, unset = NA, names = TRUE))
    restore(old)
    show(identical(Sys.getenv(names, unset = NA, names = TRUE), before))
}
invisible(Sys.unsetenv(names))
show(Sys.getenv(names, unset = NA))
