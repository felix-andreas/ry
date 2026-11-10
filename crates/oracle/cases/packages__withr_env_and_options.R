# oracle: requires withr
# withr's local_envvar(), local_options(), local_language() and with_*() helpers: set, unset,
# empty, NA and non-ASCII values, repeated names, NULL options, restoring on exit, and the errors
# for unnamed or invalid input.
library(withr)
show <- function(expr) {
    value <- tryCatch(expr, error = function(e) paste("error:", conditionMessage(e)))
    str(value)
}
Sys.setenv(RY_W_A = "start")
Sys.unsetenv(c("RY_W_B", "RY_W_C"))
f <- function() {
    old <- local_envvar(RY_W_A = "inner", RY_W_B = "été", RY_W_C = NA)
    str(old)
    show(Sys.getenv(c("RY_W_A", "RY_W_B", "RY_W_C"), unset = NA))
    local_envvar(c(RY_W_A = "second", RY_W_A = "last"))
    show(Sys.getenv("RY_W_A"))
    local_envvar(list(RY_W_C = "", RY_W_B = NULL))
    show(Sys.getenv(c("RY_W_B", "RY_W_C"), unset = NA))
    invisible()
}
f()
show(Sys.getenv(c("RY_W_A", "RY_W_B", "RY_W_C"), unset = NA))
show(with_envvar(c(RY_W_A = "w"), Sys.getenv("RY_W_A")))
show(with_envvar(c(RY_W_A = "p"), Sys.getenv("RY_W_A"), action = "prefix"))
show(with_envvar(c(RY_W_A = "s"), Sys.getenv("RY_W_A"), action = "suffix"))
show(local_envvar(c("unnamed")))
show(with_envvar(list(RY_W_A = 1), Sys.getenv("RY_W_A")))
show(with_envvar(list(), Sys.getenv("RY_W_A")))
show(withr:::get_envvar(c(RY_W_A = "x", RY_W_NONE = "y")))
show(withr:::as_envvars(list(a = NULL, b = "x", a = "y")))
show(withr:::as_envvars(c(a = "1", "2")))
show(withr:::list_combine(list(a = 1, b = 2), list(b = NULL, c = 3, c = 4)))
show(withr:::list_combine(list(1, 2), list(z = "z")))
show(withr:::list_combine(list(a = 1), list()))
g <- function() {
    old <- local_options(ry.w.one = 1, ry.w.two = "two", digits = 4)
    str(old)
    print(pi)
    local_options(list(ry.w.one = NULL))
    show(getOption("ry.w.one"))
    invisible()
}
g()
show(getOption("ry.w.one")); show(getOption("ry.w.two")); print(pi)
show(with_options(list(width = 30), getOption("width")))
show(local_options(digits = 30))
show(getOption("digits"))
show(withr:::get_options(list(digits = 1, ry.w.none = 2)))
show(withr:::get_options(c(width = "w")))
show(withVisible(withr:::set_options(list(ry.w.three = 3)))$visible)
show(withr:::reset_options(list(ry.w.three = NULL)))
show(withr:::reset_options(list()))
h <- function() {
    # Without NLS (ry) local_language() warns that it has no effect, and GNU R may warn about
    # LC_ALL, so the warnings are not part of the comparison.
    suppressWarnings(local_language("fr", .local_envir = environment()))
    show(Sys.getenv("LANGUAGE"))
    show(dir.exists(tempdir()))
    cache <- withr:::reset_gettext_cache()
    show(is.null(cache) || is.character(cache))
}
Sys.setenv(LANGUAGE = "en")
h()
show(Sys.getenv("LANGUAGE"))
Sys.unsetenv(c("RY_W_A", "LANGUAGE"))
