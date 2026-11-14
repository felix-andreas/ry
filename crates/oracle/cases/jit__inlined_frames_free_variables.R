# A call the optimizing tier runs without a frame reads a free variable where the enclosing
# environments held it when the caller was compiled. Between two such calls the binding can be
# reassigned, removed, shadowed by a nearer environment, moved by a growing frame, made active
# or delayed, and the closure can get another environment.

scale <- 2
weigh <- function(x) x * scale
poke_global <- function(step, ...) {
  if (step == 20L) assign("scale", 3, envir = globalenv())
  if (step == 30L) {
    rm("scale", envir = globalenv())
    makeActiveBinding("scale", function() 5, globalenv())
  }
  if (step == 40L) {
    rm("scale", envir = globalenv())
    delayedAssign("scale", { cat("forced at", step, "\n"); 7 }, assign.env = globalenv())
  }
  if (step == 50L) rm("scale", envir = globalenv())
  step
}
on_global <- function(n) {
  total <- 0
  for (i in seq_len(n)) {
    poke_global(i)
    total <- total + weigh(i)
  }
  total
}
for (k in 1:30) weigh(k)
for (k in 1:40) on_global(10L)
print(on_global(45L))
print(tryCatch(on_global(60L), error = function(e) conditionMessage(e)))
scale <- 2
print(on_global(12L))

rate <- 100
make <- function() {
  rate <- 2
  function(x) x * rate
}
priced <- make()
poke_frame <- function(step, ...) {
  frame <- environment(priced)
  if (step == 20L) assign("rate", 3, envir = frame)
  if (step == 30L) rm("rate", envir = frame)
  if (step == 40L) {
    for (name in c(letters, LETTERS)) assign(name, 0, envir = frame)
    assign("rate", 5, envir = frame)
  }
  if (step == 50L) rm(list = letters, envir = frame)
  if (step == 60L) {
    rm("rate", envir = frame)
    makeActiveBinding("rate", function() step, frame)
  }
  if (step == 70L) {
    rm("rate", envir = frame)
    delayedAssign("rate", { cat("frame forced\n"); 11 }, assign.env = frame)
  }
  step
}
on_frame <- function(n) {
  total <- 0
  for (i in seq_len(n)) {
    poke_frame(i)
    total <- total + priced(i)
  }
  total
}
for (k in 1:30) priced(k)
for (k in 1:40) on_frame(10L)
for (n in c(25L, 35L, 45L, 55L, 65L, 75L)) {
  frame <- environment(priced)
  if (exists("rate", envir = frame, inherits = FALSE)) rm("rate", envir = frame)
  assign("rate", 2, envir = frame)
  print(on_frame(n))
}

nest <- function() {
  depth <- 1
  inner <- function() function(x) x + depth
  inner()
}
deep <- nest()
poke_nearer <- function(step, ...) {
  nearer <- environment(deep)
  if (step == 20L) assign("depth", 1000, envir = nearer)
  if (step == 30L) rm("depth", envir = nearer)
  if (step == 40L) assign("depth", 5, envir = parent.env(nearer))
  step
}
on_nearer <- function(n) {
  total <- 0
  for (i in seq_len(n)) {
    poke_nearer(i)
    total <- total + deep(i)
  }
  total
}
for (k in 1:30) deep(k)
for (k in 1:40) on_nearer(10L)
print(on_nearer(25L))
print(on_nearer(35L))
print(on_nearer(45L))

late <- function(amount) function(x) x + amount
waiting <- late({ cat("argument forced\n"); 4 })
on_promise <- function(n) {
  total <- 0
  for (i in seq_len(n)) total <- total + waiting(i)
  total
}
print(on_promise(3L))
for (k in 1:40) on_promise(10L)
print(on_promise(20L))
absent <- late()
on_missing <- function(n) {
  total <- 0
  for (i in seq_len(n)) total <- total + if (i > 15L) absent(i) else waiting(i)
  total
}
for (k in 1:40) on_missing(10L)
print(tryCatch(on_missing(20L), error = function(e) conditionMessage(e)))

base_value <- 1
moved <- function(x) x + base_value
poke_closure <- function(step, ...) {
  if (step == 20L) {
    elsewhere <- new.env()
    assign("base_value", 500, envir = elsewhere)
    environment(moved) <<- elsewhere
  }
  step
}
on_moved <- function(n) {
  total <- 0
  for (i in seq_len(n)) {
    poke_closure(i)
    total <- total + moved(i)
  }
  total
}
for (k in 1:30) moved(k)
for (k in 1:40) on_moved(10L)
print(on_moved(25L))
print(on_moved(5L))
