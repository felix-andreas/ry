# Compiled code runs a closure call without a frame when nothing can tell. A caller that passes
# one of its own formals on to such a call has not evaluated it, so the callee's read of the
# formal is where its promise is forced, however many calls the variable travels through. Each
# group makes that point observable: a callee that reads the formal on some calls only, a
# variable reassigned between the call and the caller's own read, a promise that looks at the
# frames it is forced under, a promise with an effect, and a formal nothing reads.
# Each function runs often enough for compiled code to run it and to give up what does not pay.

v <- 1
bump <- function() v <<- v + 100
inner <- function(y, c) {
  if (c < 0L) stop("negative: ", c)
  if (c > 0L) y else 0
}
middle <- function(x, c) {
  if (c < 0L) stop("negative: ", c)
  inner(x, c) + 0
}
nested <- function(a, c) {
  r <- middle(a, c)
  bump()
  r + a
}
total <- 0
for (i in 1:80) {
  v <- 1
  total <- total + nested(v, i %% 2L)
}
print(total)
v <- 1
print(nested(v, 0L))
v <- 1
print(nested(v, 1L))
print(tryCatch(nested(stop("a forced"), 0L), error = function(e) conditionMessage(e)))
print(tryCatch(nested(stop("a forced"), 1L), error = function(e) deparse(conditionCall(e))))
print(tryCatch(nested(, 1L), error = function(e) conditionMessage(e)))
print(tryCatch(nested(1, -1L), error = function(e) deparse(conditionCall(e))))

ignore <- function(y) 1
reassigning <- function(x, flag) {
  ignore(x)
  if (flag) assign("w", 99, parent.frame())
  x
}
outer <- function(flag) {
  w <- 1
  reassigning(w, flag)
}
total <- 0
for (i in 1:80) total <- total + outer(i %% 20L == 0L)
print(total)

depth <- function() sys.nframe()
callers <- function() vapply(sys.calls(), function(call) deparse(call[[1L]]), "")
scaled <- function(x, k) {
  if (x < 0L) stop("negative")
  x * k
}
scale_by <- function(i, k) {
  r <- scaled(i, k)
  r + k
}
total <- 0
for (i in 1:80) total <- total + scale_by(i, depth())
print(total)
print(scale_by(2L, depth()))
named <- function(x, who) {
  if (x < 0L) stop("negative")
  if (x > 1L) who else "nobody"
}
name_of <- function(i, who) named(i, who)
for (i in 1:60) name_of(i %% 3L, callers())
print(name_of(2L, callers()))
print(name_of(0L, callers()))
print(deparse(name_of(2L, sys.call())))
print(deparse(name_of(2L, match.call(scaled, quote(scaled(1, 2))))))

seen <- 0L
pick <- function(x, k) {
  if (x < 0L) stop("negative")
  if (x > 50L) x * k else x
}
pass <- function(i, k) pick(i, k)
total <- 0
for (i in 1:80) {
  total <- total + pass(i + 10L, {
    seen <- seen + 1L
    3
  })
}
print(c(total, seen))

unread <- function(x, unused) {
  if (x > 70L) return(list(missing(unused), substitute(unused), nargs()))
  x + 1L
}
hand_on <- function(i, u) unread(i, u)
total <- 0L
for (i in 1:60) total <- total + hand_on(i, stop("never forced"))
print(total)
str(hand_on(100L, stop("never forced")))
str(hand_on(100L))
