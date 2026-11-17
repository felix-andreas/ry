# A pairlist's elements never break the line, although a vector or a call inside one does.
p <- pairlist(alpha_long_name = 1:10 + 0.5, beta_long_name = "a string that is quite long indeed",
              gamma = c(TRUE, NA, FALSE), delta = quote(some_function(with, arguments)), 5,
              epsilon = NULL)
deparse(p)
deparse(p, width.cutoff = 20)
deparse(as.call(list(as.name("f"), p)), width.cutoff = 20)
p

# `...` in a matched call is a pairlist.
f <- function(...) match.call(expand.dots = FALSE)
deparse(f(2.5, 0, NA_complex_, "a long string to pass the width cutoff of sixty characters", 1:20 + 0))
f(first = "a long string to pass the width cutoff", second = "another long string to pass it", 3)

# A pairlist with an empty element is written through `alist()`, on one line as well.
deparse(as.pairlist(alist(a = , b = 2, cccccccccccccccccccccccccccccc = "dddddddddddddddddddddddddd",
                          e = , fffffffffffffffffffffff = 3)))
deparse(formals(function(aaaaaaaaaaaaaaaaaaaaaaaaaaa = 1, bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb = 2,
                         cccccccccccccccccccccccccc = 3, ddddddd) NULL))

# A call's own arguments break as before.
deparse(quote(f(alpha_long_name = 1, beta_long_name = "a string that is quite long indeed", gamma = 3)))
