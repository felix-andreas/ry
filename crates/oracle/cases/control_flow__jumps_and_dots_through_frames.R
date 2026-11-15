# Code evaluated in a caller's frame from outside it, while the caller runs compiled: a break
# aimed at the caller's loop, `..N` read from the caller's dots, and a break in a promise
# argument that a callee forces. Each function runs many times so its body compiles.

# A break evaluated in parent.frame() ends the caller's loop, whose body has a break inside
# another call, so the loop takes jumps from elsewhere.
stop_caller <- function() eval(quote(break), parent.frame())
ignored <- function(x) NULL
looping <- function(n) {
  count <- 0
  for (i in 1:n) {
    count <- count + 1
    if (i == 3) stop_caller()
    ignored(break)
  }
  count
}
for (k in 1:30) r <- tryCatch(looping(5), error = function(e) conditionMessage(e))
print(r)

# match.call writes the forwarded dots as ..1, ..2, and eval.parent evaluates them in the frame
# that owns the dots, whose body only passes `...` on.
target <- function(x, file, ...) paste(x, nchar(file))
rebuild <- function(...) {
  call <- match.call(expand.dots = TRUE)
  call[[1L]] <- quote(target)
  eval.parent(call)
}
forward <- function(...) rebuild(..., flag = FALSE)
through <- function(value, file) forward(value, file)
for (k in 1:30) r <- tryCatch(through(k, "abcd"), error = function(e) conditionMessage(e))
print(r)
read_dot <- function() eval(quote(..2), parent.frame())
dots_owner <- function(...) read_dot()
for (k in 1:30) r <- tryCatch(dots_owner(k, k * 2), error = function(e) conditionMessage(e))
print(r)

# A break or next in an argument unwinds out of the callee that forces it and lands in the
# caller's loop.
identity_of <- function(v) v
summing <- function(m) {
  s <- 0
  for (i in 1:m) s <- s + identity_of(if (i == 4) break else if (i == 2) next else i)
  s
}
for (k in 1:30) r <- summing(6)
print(r)
