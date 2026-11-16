# A call keeps its argument names as tags, and `names<-` with NULL drops them (dtplyr's
# `unname(call_match(...))`).
x <- quote(f(test = a, yes = 1, no = 2))
names(x) <- NULL
print(x)
print(names(x))
print(unname(quote(g(a = 1, 2))))
p <- pairlist(a = 1, b = 2)
names(p) <- NULL
print(names(p))
