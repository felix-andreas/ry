fact <- function(n) if (n <= 1) 1 else n * fact(n - 1)
fact(10); fact(20)
fib <- function(n) if (n < 2) n else Recall(n - 1) + Recall(n - 2)
sapply(0:10, fib)
hanoi_moves <- function(n) if (n == 0) 0 else 2 * hanoi_moves(n - 1) + 1
hanoi_moves(10)
flatten <- function(x) if (!is.list(x)) list(x) else do.call(c, lapply(x, flatten))
unlist(flatten(list(1, list(2, list(3, 4)), 5)))
ackermann <- function(m, n) if (m == 0) n + 1 else if (n == 0) ackermann(m - 1, 1) else ackermann(m - 1, ackermann(m, n - 1))
ackermann(2, 3)
depth <- function(x) if (is.list(x) && length(x)) 1 + max(sapply(x, depth)) else 0
depth(list(1, list(2, list(3))))
tryCatch((function(n) sys.function()(n))(1), error = function(e) "infinite")
# The recalled call's parent is the caller of the function that called `Recall`, and its call is
# that function's call.
up <- function(n) if (n <= 0) parent.frame() else Recall(n - 1)
home <- function() { e <- environment(); identical(up(2), e) }
home()
called <- function(n) if (n <= 1) sys.call() else Recall(n - 1)
called(3)
