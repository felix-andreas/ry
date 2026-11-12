# The evaluator writes an operator's result into the fresh result of an inner operator call,
# and must leave every value something else can still see untouched.
x <- c(1, 2, 3, 4)
y <- x * 2 + 1
print(x)
print(y)

# Unary plus returns its operand, so the operand embedded in a constructed call must survive.
v <- c(10, 20, 30)
e <- call("*", call("+", v), 2)
print(eval(e))
print(e[[2]][[2]])

# A binary operator call with an embedded vector still allocates a fresh result.
e2 <- call("+", call("*", v, 1), 1)
print(eval(e2))
print(e2[[2]][[2]])

# A user operator bound in the frame may return its operand.
local({
  `*` <- function(a, b) a
  w <- c(5, 6, 7)
  print(w * 2 + 1)
  print(w)
})

# Mixed types: an integer temporary cannot hold a double result.
print((1:4 * 2L) + 0.5)
print((c(1.5, 2.5) * 2) > 3)
print((1:3 * 2L) + 1i)

# Both operands temporaries of different types.
print((1:3 * 2L) + (c(1, 2, 3) * 0.5))

# Classed values dispatch first and see their operands unchanged.
Ops.keep <- function(e1, e2) {
  r <- get(.Generic)(unclass(e1), unclass(e2))
  structure(r, class = "keep")
}
k <- structure(c(1, 2, 3), class = "keep")
print(unclass(k * 2 + 1))
print(unclass(k))

# Named and dimensioned temporaries keep their attributes.
n <- c(a = 1, b = 2, c = 3)
print(n * 2 + 1)
m <- matrix(1:6 + 0, 2)
print(m * 2 + 1)
print(sqrt(x * 4))
print(-(x * 2))
f <- function(a) a * 2 + 1
print(f(x))
print(x)
