# A call combined into a list is one element, so its argument tags do not name the result.
dput(unlist(list(quote(s(log(d), k = 4)))))
dput(unlist(list(quote(f(a = 1)), quote(g(b = 2)))))
dput(unlist(list(pairlist(a = 1))))
dput(c(list(1), quote(f(a = 1))))
dput(unlist(list(x = quote(f(a = 1)))))
dput(c(quote(f(a = 1))))
dput(unlist(list(1, pairlist(a = 1, 2))))
