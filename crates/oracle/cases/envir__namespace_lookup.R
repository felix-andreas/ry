# `pkg::name` and `pkg:::name` with symbols and strings on either side, exported, internal and
# lazy-data names, the errors for names that are missing or not exported, getExportedValue(),
# and options() setting, reading and removing several options in one call.
show <- function(expr) {
    value <- tryCatch(expr, error = function(e) paste("error:", conditionMessage(e)))
    if (is.function(value)) value <- "<function>"
    print(value)
}

show(base::paste("a", "b"))
show("base"::paste("a", "b"))
show(base::"paste"("a", "b"))
show(stats::median(c(3, 1, 2)))
show(stats:::median.default(c(3, 1, 2)))
show("stats":::"median.default"(c(5, 4)))
show(utils::head(letters, 2))
show(datasets::mtcars[1:2, 1:3])
show(stats::no_such_function)
show(stats:::no_such_function)
show(stats::median.default)
show(base::no_such_function)
show(getExportedValue("stats", "median"))
show(getExportedValue(asNamespace("stats"), "sd")(c(1, 3)))
show(identical(base::`+`, `+`))
f <- function() stats::sd(c(2, 4, 6))
show(vapply(1:3, function(i) f(), 0))

old <- options(ry.first = 1, ry.second = "two", digits = 9)
print(old)
print(options("ry.first", "ry.second", "digits"))
print(getOption("ry.second"))
print(options(list(ry.first = NULL, ry.third = TRUE)))
print(options("ry.first", "ry.third"))
show(options(digits = 30))
print(getOption("digits"))
options(old)
options(ry.third = NULL)
print(options("ry.first", "ry.second", "ry.third", "digits"))
