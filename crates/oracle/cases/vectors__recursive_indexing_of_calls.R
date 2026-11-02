# Recursive [[ descends through calls and pairlists as well as lists.
e <- quote(f(a, g(b, h(c, d))))
e[[c(3, 3)]]
e[[c(3, 3, 2)]]
fn <- quote(function(x, y = 1) { x; y })
fn[[c(2, 2)]]
fn[[c(3, 3)]]
x <- e
x[[c(3, 3, 2)]] <- quote(zz)
x
fn[[c(2, 2)]] <- quote(Z)
fn
