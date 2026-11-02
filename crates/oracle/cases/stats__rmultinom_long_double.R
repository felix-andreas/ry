# rmultinom carries the remaining probability with more than double precision, as GNU R's
# long double does, so the conditional binomials match draw for draw.
set.seed(39)
print(rmultinom(5, 100, rep(1/10, 10)))
set.seed(39)
print(rmultinom(2, 1e5, 1:10))
