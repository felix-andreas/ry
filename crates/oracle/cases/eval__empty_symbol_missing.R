# The empty symbol is R's missing argument, which the base frame binds to itself. Evaluating it
# is a missing argument without a name, not an unbound object, however the empty symbol reaches
# `eval`, wherever the lookup reaches base.
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

# Below an environment whose chain ends before base, the empty symbol is unbound, which is how
# rlang's empty quosure fails.
show(eval(quote(expr = ), new.env(parent = emptyenv())))
show(eval(quote(expr = ), new.env(parent = new.env(parent = emptyenv()))))
show(eval(quote(expr = ), new.env(parent = baseenv())))
