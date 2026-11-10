# Names that reach the runtime as interned strings or symbols: options set and read by name, `::` with symbol and string operands, `typeof`, builtin dispatch on `$`, `$<-` and `seq`, replacement calls with extra arguments, and strings whose text is not valid UTF-8 or carries a Latin-1 or bytes mark.
sh <- function(expr) tryCatch(expr, error = function(e) cat("Error:", conditionMessage(e), "\n"))
old <- options(ry.test.one = 1, ry.test.two = "b")
str(old)
str(options("ry.test.one", "ry.test.missing"))
str(options(c("ry.test.two", "ry.test.one"))); sh(options(character()))
str(options(ry.test.one = NULL))
str(getOption("ry.test.one"))
str(options(list(ry.test.three = 3)))
options(ry.test.two = NULL, ry.test.three = NULL)
print(base::paste("a", "b")); print("base"::nchar("abc")); print(utils::head(1:5, 2))
print(stats:::median.default(c(3, 1, 2)))
sh(base::not_a_function); sh(stats::median.default.nope); sh(notapackage::f)
print(identical(getExportedValue("stats", "median"), stats::median))
print(identical(get("sd", envir = asNamespace("stats")), stats::sd))
for (value in list(NULL, quote(x), pairlist(1), function() 1, globalenv(), quote(f(x)), sum, `if`,
                   TRUE, 1L, 1, 1i, "a", list(), expression(1), as.raw(1)))
  cat(typeof(value), "")
cat("\n")
cat(typeof(asS4(structure(list(), class = "x"))), "\n")
obj <- structure(list(a = 1), class = "rec")
`$.rec` <- function(x, name) paste("get", name)
`$<-.rec` <- function(x, name, value) { y <- unclass(x); y[[name]] <- value * 10; structure(y, class = "rec") }
print(obj$anything)
obj$b <- 2
print(unclass(obj))
seq.rec <- function(...) "seq.rec"
print(seq(obj)); print(seq.int(1, 3))
x <- c(a = 1, b = 2, c = 3)
names(x)[2] <- "B"; attr(x, "note") <- "n"; substr(names(x)[1], 1, 1) <- "Z"
print(x)
`second<-` <- function(x, at = 2, value) { print(sys.call()); x[at] <- value; x }
second(x, 3) <- 30; second(x) <- 20
print(x)
l <- list(a = list(b = 1))
l$a$b <- 2; l[["a"]][["c"]] <- 3
str(l)
f <- function(type = c("linear", "quadratic", "cubic")) match.arg(type)
print(f()); print(f("quad")); sh(f("nope"))
print(Reduce(`+`, 1:4, accumulate = FALSE)); print(Reduce(function(a, b) paste0(a, b), letters[1:3], "z", right = TRUE))
stopifnot(TRUE, c(TRUE, TRUE)); sh(stopifnot("must be true" = FALSE))
print(format(3.14159, digits = 3)); print(format(c(1, 10, 100), width = 6)); print(format(TRUE))
latin <- "caf\xe9"; Encoding(latin) <- "latin1"
bytes <- "caf\xe9"; Encoding(bytes) <- "bytes"
utf8 <- "café"
invalid <- "a\xffb"
print(c(latin, utf8)); print(bytes); print(invalid)
print(Encoding(c(latin, bytes, utf8, invalid))); print(nchar(c(latin, utf8))); print(latin == utf8)
print(toupper(latin)); print(substr(utf8, 4, 4)); print(enc2utf8(latin) == utf8)
print(names(c(`café` = 1))); print(as.character(as.name(utf8)))
e <- new.env(); assign(utf8, 1, envir = e); print(ls(e)); print(exists(enc2utf8(latin), envir = e))
print(paste(invalid, "x")); print(nchar(invalid, "bytes")); print(validUTF8(c(invalid, utf8, latin)))
