# A compiled top-level loop reads, writes and checks its global names through cached binding
# cells. Every way a name's binding can change in the middle of such a loop must show in the
# iterations after it, and a value the loop updates must not change once something else holds it.

# A callee defines a global that shadows a base binding.
shadow <- function() assign("T", 0, envir = globalenv())
total <- 0
for (i in 1:40) { if (i == 20) shadow(); total <- total + T }
print(total)
rm(T)

# A callee removes the global, and the lookup falls through to base.
pi <- 0
unshadow <- function() rm("pi", envir = globalenv())
total <- 0
for (i in 1:40) { if (i == 20) unshadow(); total <- total + pi }
print(total)

# A callee assigns an existing global, through `assign`, `<<-` and `eval` into the global env.
step <- 1
total <- 0
for (i in 1:40) {
  if (i == 10) assign("step", 10, envir = globalenv())
  if (i == 20) (function() step <<- 100)()
  if (i == 30) eval(quote(step <- 1000), globalenv())
  total <- total + step
}
print(total)

# `list2env` and `delayedAssign` replace a global the loop reads, the second with a promise.
step <- 1
total <- 0
for (i in 1:40) {
  if (i == 10) list2env(list(step = 10), envir = globalenv())
  if (i == 20) delayedAssign("step", 100, assign.env = globalenv())
  total <- total + step
}
print(total)

# The loop variable itself is removed or reassigned in the body.
for (i in 1:5) { if (i == 3) rm(i) }
print(i)
seen <- integer()
for (i in 1:5) { seen <- c(seen, i); i <- 100L }
print(c(seen, i))

# A callee defines the loop's own accumulator in a fresh binding after removing it.
total <- 0
reset <- function() { rm("total", envir = globalenv()); assign("total", -1000, envir = globalenv()) }
for (i in 1:40) { if (i == 20) reset(); total <- total + 1 }
print(total)

# `sys.source` defines a global in the middle of a loop.
source_file <- tempfile(fileext = ".R")
writeLines("offset <- 1000", source_file)
offset <- 0
total <- 0
for (i in 1:40) { if (i == 20) sys.source(source_file, envir = globalenv()); total <- total + offset }
print(total)
unlink(source_file)

# Attaching a frame that binds a name the loop reads, then detaching it.
total <- 0
for (i in 1:40) {
  if (i == 10) attach(list(attached_value = 5), name = "loop_frame")
  if (i == 30) detach("loop_frame")
  total <- total + if (exists("attached_value")) attached_value else 0
}
print(total)

# An attached frame that masks `+`, as `library()` of a package that exports one does, is
# respected until it is detached in the middle of the loop. GNU R binds `+` and `<-` when it
# compiles the loop, so a mask attached after that does not show; this case keeps to what both
# agree on.
attach(list(`+` = function(a, b) base::`-`(a, b)), name = "masking_plus")
total <- 0
for (i in 1:40) { if (i == 10) detach("masking_plus"); total <- total + 1 }
print(total)

# The same for `<-`.
counted <- 0
attach(list(`<-` = function(x, value) {
  counted <<- counted + 1
  eval.parent(substitute(base::`<-`(x, value)))
}), name = "masking_assign")
for (i in 1:40) { if (i == 10) detach("masking_assign"); last <- i }
print(c(last, counted))

# A function the loop calls, masked by an attached frame in the middle of the loop.
total <- 0
for (i in 1:40) {
  if (i == 10) attach(list(sqrt = function(x) 100), name = "masking_sqrt")
  total <- total + sqrt(1)
}
print(total)
detach("masking_sqrt")

# `makeActiveBinding` on a new global the loop then reads.
total <- 0
for (i in 1:40) {
  if (i == 20) makeActiveBinding("active_value", function() 2, globalenv())
  total <- total + if (exists("active_value", inherits = FALSE)) active_value else 1
}
print(total)
rm(active_value)

# `lockBinding` on the accumulator stops the loop with the binding's error.
locked <- 0
result <- tryCatch(
  for (i in 1:40) { if (i == 20) lockBinding("locked", globalenv()); locked <- locked + 1 },
  # GNU R's message here is "bad binding access", from reading an unboxed binding cell.
  error = function(e) "stopped"
)
print(result)
print(locked)
unlockBinding("locked", globalenv())

# Values the loop updates, captured mid-loop: the captured value must not change afterwards.
s <- 0
for (i in 1:40) { s <- s + i; if (i == 20) captured <- s }
print(c(s, captured))
s <- 0
for (i in 1:40) { s <- s + i; if (i == 20) held <- list(s) }
print(c(s, held[[1]]))
s <- 0
for (i in 1:40) { s <- s + i; if (i == 20) read_s <- local({ value <- s; function() value }) }
print(c(s, read_s()))
s <- 0
for (i in 1:40) { s <- s + i; if (i == 20) promised <- (function(x) function() x)(s) }
print(c(s, promised()))
for (i in 1:40) if (i == 20) captured_i <- i
print(c(i, captured_i))
for (i in 1:40) if (i == 20) held_i <- list(i)
print(c(i, held_i[[1]]))
for (i in 1:40) if (i == 20) closure_i <- local({ value <- i; function() value })
print(c(i, closure_i()))

# An enclosing call that holds the accumulator's value while code it evaluates updates it.
s <- 0
held <- c(s, eval(quote({ for (i in 1:3) s <- s + i; s }), globalenv()))
print(held)

# The constant an accumulator starts from is the same on every run of the outer loop.
for (k in 1:2) { s <- 0; for (i in 1:3) s <- s + i; print(s) }

# Integer accumulators: an overflow gives NA with a warning, and a double operand makes the
# result a double.
s <- 1L
for (i in 1:3) s <- s * 1000000L
print(s)
s <- 0L
for (i in 1:5) s <- s + i
print(c(s, typeof(s)))
s <- 0L
for (i in 1:3) s <- s + 0.5
print(s)

# Other ways to hold the value: a function's result, `get`, `mget`, an alias, and the value
# of the assignment itself.
f <- function() s
s <- 0
for (i in 1:5) { s <- s + i; if (i == 2) from_function <- f() }
print(c(s, from_function))
s <- 0
for (i in 1:5) { s <- s + i; if (i == 2) from_get <- get("s", envir = globalenv()) }
print(c(s, from_get))
s <- 0
for (i in 1:5) { s <- s + i; if (i == 2) from_mget <- mget("s") }
print(c(s, from_mget$s))
s <- 0
for (i in 1:5) { s <- s + i; if (i == 2) { alias <- s; alias <- alias } }
print(c(s, alias))
s <- 0
for (i in 1:5) { value <- (s <- s + i) }
print(c(s, value))

# A named scalar keeps its name.
s <- c(a = 1)
for (i in 1:3) s <- s + i
print(s)
