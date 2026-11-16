# `nchar` keeps the names of its argument, and a pairlist's tags count as names.
print(nchar(pairlist(a = "xy", b = "z")))
print(nchar(as.pairlist(alist(i = ))))
print(nchar(c(p = "abc", q = "")))
expr <- quote(function(i) a[i] * x)
print(sapply(expr, function(it) identical(nchar(it), 0L)))
