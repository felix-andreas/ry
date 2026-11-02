x <- list(a = 1, 2:3)
names(x)[2] <- NA
print(names(unlist(x)))
print(names(unlist(list(a = list(1, 2), b = 3))))
y <- alist(a = , b = )
names(y) <- c("", NA)
dput(y)
dput(y, control = "all")
d <- function(x, ...) {
  r <- tryCatch(deparse(x, control = c(...)), warning = function(w) conditionMessage(w))
  cat(r, sep = "\n")
}
d(y ~ x, "warnIncomplete")
d(new.env(), "warnIncomplete")
d(list(e = new.env()), "warnIncomplete")
d(structure(1, e = globalenv()), "warnIncomplete")
d(structure(1, e = globalenv()), "warnIncomplete", "showAttributes")
d(quote(f(x)), "warnIncomplete")
d(function(x) x, "warnIncomplete")
