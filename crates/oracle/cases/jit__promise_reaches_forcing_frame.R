# The code of an argument's promise can reach the frame of the function that forces it
# (`sys.frame()`). It sees every variable the function has assigned by then, and a variable it
# assigns there is the value the function reads afterwards. Each function runs often enough to
# be compiled before the call that reaches into its frame.

frame_of <- function(f) {
  for (k in seq_len(sys.nframe())) if (identical(sys.function(k), f)) return(sys.frame(k))
  NULL
}

# The promise lists and reads a local that the function assigned before forcing it.
count <- function(expr, names) {
  nh <- length(names)
  if (nh > 1L) 1 else if (nh == 1L) 2 else expr
}
for (i in 1:5000) count(0, character())
print(count(ls(frame_of(count)), character()))
print(count(get("nh", frame_of(count)), character()))
print(count(0, character()))

# The promise of a function argument is forced by the call.
apply_to <- function(x, FUN) {
  n <- length(x) + 1L
  FUN(n)
}
for (i in 1:5000) apply_to(1:3, identity)
print(apply_to(1:3, {
  seen <- get("n", frame_of(apply_to))
  function(v) c(v, seen)
}))

# An element assignment forces the promise on the path that has not read the argument.
fill <- function(v, flag) {
  m <- length(flag) + 2L
  if (flag) v[1L]
  v[2L] <- 9
  c(v, m)
}
for (i in 1:5000) fill(c(1, 2), TRUE)
print(fill({
  print(get("m", frame_of(fill)))
  c(5, 6)
}, FALSE))

# The promise assigns a local of the function that forces it, in a frame without and with a
# closure made over it.
keep <- function(expr, a) {
  nh <- a + 1L
  expr
  nh + 1L
}
for (i in 1:5000) keep(0, 1L)
for (round in 1:3) {
  print(keep(assign("nh", 99L, envir = frame_of(keep)), 1L))
  print(keep(eval(quote(nh <- 98L), frame_of(keep)), 1L))
  print(keep(0, 1L))
}
held <- function(expr, a) {
  nh <- a
  reader <- function() nh
  expr
  c(nh, reader())
}
for (i in 1:5000) held(0, 1L)
for (round in 1:3) {
  print(held(assign("nh", 99L, envir = frame_of(held)), 1L))
  print(held(eval(quote(nh <- 98L), frame_of(held)), 1L))
  print(held(0, 1L))
}

# A default argument that binds a variable of its own frame.
sized <- function(x, n = { y <- x; length(y) }) {
  s <- 1L
  s + n + length(x) + length(y)
}
for (i in 1:5000) sized(1:3)
print(sized(1:3))
print(sized(1:3, {
  assign("s", 10L, envir = frame_of(sized))
  assign("y", 1:5, envir = frame_of(sized))
  2L
}))
