# `all.vars` and `all.names(functions = FALSE)` skip the whole function position of a call,
# even when it is a call itself; the limit counts names after duplicates are dropped.
expressions <- list(quote((function(p) p)(q)), quote(f(x)(y)), quote(a$b(c)), quote(x[[1]](y)),
    quote(f(g(x), h(y)(z))), quote(x + x + y), expression(a + b, f(c)), quote(function(x, y = z) x + w),
    quote(`é` + `a b`), quote(f()), quote(x), 1, NULL, list(quote(x)), quote(base::f(x)), quote(-x), quote((x)),
    quote({ x; y }), quote(x <- y), quote(f(a = b, c)), as.call(list(1, quote(x))),
    as.call(list(quote(f), quote(expr = ), quote(x))))
for (expression in expressions) {
    print(all.vars(expression))
    print(all.names(expression))
    print(all.names(expression, functions = FALSE, unique = TRUE))
}
x <- quote(x + x + y + z(w))
print(all.names(x, max.names = 2))
print(all.names(x, max.names = 2, unique = TRUE))
print(all.names(x, max.names = 0))
print(all.names(x, max.names = -1))
print(all.names(x, max.names = -2))
print(all.names(x, max.names = NA))
print(all.names(x, max.names = 2.9))
print(all.names(x, unique = NA))
print(all.names(x, functions = NA))
print(all.vars(x, max.names = 1))
print(all.vars(x, unique = FALSE))
print(all.vars(x, functions = TRUE))
print(Encoding(all.names(quote(`é` + 1))))
print(all.vars(~ a + b | c))
print(all.vars(y ~ log(x) + I(z^2)))
print(tryCatch(all.names(quote(x), max.names = "a"), warning = conditionMessage))
