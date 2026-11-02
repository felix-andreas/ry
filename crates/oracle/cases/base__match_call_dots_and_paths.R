# match.call writes a ... argument given as a symbol or call as ..1, ..2, so a call rebuilt from
# it evaluates those in the frame that owns the dots.
inner <- function(x, file = "", ...) { Call <- match.call(expand.dots = TRUE); print(Call); Call }
outer <- function(...) inner(..., row.names = FALSE)
w2 <- function(value, file) outer(value, file)
cl <- w2(1, "f")
str(as.list(cl))
my_write <- function(...) write.csv(..., row.names = FALSE)
g <- function(fn) { w <- fn; function(v, f) w(v, f) }
h <- g(my_write)
f <- tempfile(fileext = ".csv")
print(tryCatch(h(mtcars, f), error = function(e) conditionMessage(e)))
w2 <- function(value, file) my_write(value, file)
print(tryCatch(w2(mtcars, f), error = function(e) conditionMessage(e)))

# Symbol names are limited to 10000 bytes, however the symbol is made.
long <- strrep("a", 10001)
print(tryCatch(do.call(list, setNames(list(1), long)), error = function(e) conditionMessage(e)))
print(tryCatch(as.name(long), error = function(e) conditionMessage(e)))

# file("") opens an anonymous file for writing and reading.
con <- suppressWarnings(file("", open = "r")); writeLines("x", con); print(readLines(con)); close(con)
r <- tryCatch(file("", open = "r"), warning = function(w) paste("W:", conditionMessage(w)), error = function(e) paste("E:", conditionMessage(e)))
print(r)
print(tryCatch(normalizePath(NA, mustWork = TRUE), error = function(e) conditionMessage(e)))
print(tryCatch(normalizePath(NA_character_, mustWork = TRUE), error = function(e) conditionMessage(e)))
print(normalizePath(c(NA, "/tmp"), mustWork = FALSE))
print(tryCatch(path.expand(NA), error=function(e) conditionMessage(e))); print(tryCatch(path.expand(1), error=function(e) conditionMessage(e)))
