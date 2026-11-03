# rapply and eapply pass ... on to FUN.
f <- function(x, add) x + add
print(rapply(list(1, list(2, 3)), f, how = "unlist", add = 10))
e <- new.env()
assign("a", 1, envir = e)
assign("b", 2, envir = e)
r <- eapply(e, f, add = 100)
print(r[order(names(r))])
print(is.null(names(eapply(e, f, add = 1, USE.NAMES = FALSE))))

# substitute accepts a list or pairlist as its environment.
print(substitute(a + b, list(a = 1, b = quote(z))))
print(substitute(a + b, as.pairlist(list(a = 2))))
