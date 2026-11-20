# `||` and `&&` reject an operand that is not logical-like, naming the operator call.
show <- function(expr) tryCatch(expr, error = function(e) conditionMessage(e))
show(FALSE || NULL)
show(NULL || TRUE)
show(TRUE && NULL)
show(FALSE || "TRUE")
show(sort(c(3, NA, 1), na.last = NULL))

# Base bindings are locked.
show(assign("toupper", 1, envir = baseenv()))
bindingIsLocked("toupper", baseenv())

# `pmin` and `pmax` keep the type of their arguments and give NULL for NULL.
str(pmax(1:3, integer(0), 2L))
str(pmin(NULL))

# `attributes<-` validates `comment` and stores row names as GNU R does.
x <- 1:3
show(attributes(x) <- list(comment = 1))
y <- list(a = 1:3)
attributes(y) <- list(names = "a", row.names = 1:3, class = "data.frame")
.row_names_info(y, type = 0L)

# Complex numbers share significant digits between the two parts.
print((1+2i)^0.5)
print((-1+0i)^0.5)
print(c(1.5+2.25i, 1/3+0i))

# A warning under `NextMethod` names the `NextMethod` call.
"[<-.tagged" <- function(x, i, value) NextMethod("[<-")
z <- structure(c(1, 2, 3), class = "tagged")
withCallingHandlers(z[] <- c(1, 2), warning = function(w) {
  print(conditionCall(w))
  invokeRestart("muffleWarning")
})

# A failed `UseMethod` names the generic's call.
g <- function(x) UseMethod("zz")
tryCatch(g(1), error = function(e) print(conditionCall(e)))

# A native and a UTF-8 string with the same bytes are unequal in the C locale.
a <- "caf\xc3\xa9"
b <- a
Encoding(b) <- "UTF-8"
old <- Sys.setlocale("LC_CTYPE", "C")
identical(a == b, TRUE)
invisible(Sys.setlocale("LC_CTYPE", old))
