# The empty symbol is R's missing argument. Evaluating it is a missing argument without a name,
# not an unbound object, however the empty symbol reaches `eval`.
show <- function(expr) tryCatch(expr, error = function(e) {
  print(conditionMessage(e))
  print(conditionCall(e))
})
show(eval(quote(expr = )))
show(eval(formals(function(x) x)$x))
e <- new.env()
show(eval(alist(a = )$a, e))
show(eval(as.symbol("")))
inside <- function(x) eval(quote(expr = ))
show(inside(1))

# match.arg with no default for `arg` evaluates the empty default the same way.
bare <- function(type) match.arg(type)
show(bare("a"))

# A named missing argument keeps its name.
f <- function(x) x
show(f())
h <- function() {
  y <- quote(expr = )
  y
}
show(h())

# Quoting and returning the empty symbol does not evaluate it.
print(identical(evalq(quote(expr = )), quote(expr = )))
print(is.name(alist(a = )$a))
