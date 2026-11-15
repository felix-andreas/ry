# Compiled code runs a closure call without a frame when nothing can tell, and evaluates the
# call's arguments ahead of it when that has no effect. When such a call needs its frame after
# all, each formal must be bound as the call left it: forced, with the value its argument had
# when the callee read it, or not yet forced, so that a later read evaluates the argument then.
# Each group changes a variable an argument mentions between the call and a read of the formal,
# from a branch that had not run while the callers warmed up. The callees run on their own
# first, so that a caller compiled early already knows them.

read <- function(y) {
  if (y < 0) stop("negative")
  y
}
for (i in 1:30) read(1)
after_read <- function(x, flag) {
  if (flag < 0) stop("negative")
  r <- read(x)
  if (flag > 0) assign("w", 99, parent.frame())
  x + r
}
for (i in 1:30) after_read(1, 0)
read_root <- function(flag) {
  w <- 1
  after_read(w, flag)
}
for (i in 1:3000) read_root(0)
for (k in c(0, 1, 2)) print(read_root(k))

maybe <- function(a, c) {
  if (c < 0) stop("negative")
  if (c > 0) a else 0
}
for (i in 1:30) maybe(2, 1)
before_read <- function(x, c) {
  r <- maybe(x * 2, c)
  if (c == 0) assign("w", 99, parent.frame())
  x + r
}
for (i in 1:30) before_read(1, 1)
unread_root <- function(c) {
  w <- 1
  before_read(w, c)
}
for (i in 1:3000) unread_root(1)
for (k in c(1, 0, 0, 1)) print(unread_root(k))

g <- 1
global_reader <- function(x, c) {
  r <- maybe(x * 2, c)
  if (c == 0) assign("g", 99, globalenv())
  x + r
}
for (i in 1:3000) global_reader(g, 1)
for (k in c(1, 0, 0)) {
  g <- 1
  print(global_reader(g, k))
}
passing_root <- function(w, c) {
  if (c < 0) stop("negative")
  global_reader(w, c)
}
for (i in 1:3000) passing_root(g, 1)
for (k in c(1, 0, 0)) {
  g <- 1
  print(passing_root(g, k))
}

# A local the caller binds on some calls only is a global on the others.
k <- 10
double <- function(v) {
  if (v < 0) stop("negative")
  v * 2
}
for (i in 1:30) double(i)
sometimes_local <- function(n) {
  if (n > 5) k <- 2
  double(k)
}
for (i in 1:3000) sometimes_local(9)
for (n in c(9, 1, 9, 1)) print(sometimes_local(n))

# A branch that had not run registers an exit handler, which belongs to the callee's frame.
log <- character()
guarded <- function(x) {
  if (x > 90) on.exit(log <<- c(log, paste("left", x)))
  x + 1
}
for (i in 1:30) guarded(i)
guard_caller <- function(n) {
  s <- 0
  for (i in seq_len(n)) s <- s + guarded(i)
  log <<- c(log, "caller done")
  s
}
for (round in 1:5) guard_caller(80)
print(guard_caller(93))
print(log)

# An argument that had always been a plain number arrives with a class.
"+.money" <- function(e1, e2) structure(list(amount = unclass(e1)$amount + e2), class = "money")
add_fee <- function(x, fee) {
  if (fee < 0) stop("negative")
  x + fee
}
for (i in 1:30) add_fee(i, 1)
charge <- function(x) add_fee(x, 2)
for (i in 1:3000) charge(i)
print(charge(5))
print(unclass(charge(structure(list(amount = 5), class = "money"))))
print(charge(6L))

# A lazy argument forced under two rebuilt frames sees both of them.
leaf <- function(v, c) {
  if (c < 0) stop("negative")
  if (c > 0) v else 0
}
for (i in 1:30) leaf(i, 0)
branch <- function(v, c) {
  if (c < 0) stop("negative")
  leaf(v, c) + 0
}
for (i in 1:30) branch(i, 0)
trunk <- function(c) branch(c(sys.nframe(), length(sys.calls()), identical(sys.function(), trunk),
                              identical(parent.frame(), globalenv())), c)
for (i in 1:3000) trunk(0)
print(trunk(0))
print(trunk(1))
print(trunk(1))
