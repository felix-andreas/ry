# `x[i] <<- v`, `x[[i]] <<- v` and `x$name <<- v` on a variable of an enclosing function, from
# functions that run often enough to compile.
make <- function() {
  numbers <- numeric(4L)
  items <- list(1, "b")
  fields <- list(a = 1, b = "two")
  keys <- character(0)
  list(
    number = function(i, v) { numbers[i] <<- v; invisible(NULL) },
    constant = function(v) { numbers[2L] <<- v; items[["k"]] <<- v; fields$b <<- v },
    item = function(i, v) items[[i]] <<- v,
    field = function(v) fields$c <<- v,
    append = function(key) { n <- length(keys) + 1L; keys[n] <<- key },
    read = function() list(numbers, items, fields, keys)
  )
}
store <- make()
for (i in 1:6) {
  store$number(i, i * 1.5)
  store$constant(i)
  store$item(i %% 3L + 1L, letters[i])
  print(store$field(i))
  store$append(paste0("k", i))
}
str(store$read())
store$item(1L, "invisible")
(store$item(1L, "visible"))

# The subscript is a local, an argument, a variable of the enclosing function or a global, and
# a local that is assigned on some calls only.
position <- 3L
subscripts <- function() {
  kept <- c(a = 0, b = 0, c = 0, d = 0)
  inner <- 2L
  by_local <- function(v) { at <- 1L; kept[at] <<- v }
  by_argument <- function(at, v) kept[at] <<- v
  by_enclosed <- function(v) kept[inner] <<- v
  by_global <- function(v) kept[position] <<- v
  by_name <- function(name, v) kept[[name]] <<- v
  by_maybe_local <- function(v, set) { if (set) inner <- 4L; kept[inner] <<- v }
  by_default <- function(v, at = length(kept)) kept[at] <<- v
  out <- list()
  for (i in 1:5) {
    by_local(i); by_argument(4L, i * 10); by_enclosed(i * 100); by_global(-i)
    by_name("d", i / 2); by_maybe_local(i, i %% 2L == 0L); by_default(i + 0.5)
    out[[i]] <- kept
  }
  out
}
str(subscripts())

# A missing subscript, an unbound subscript and an unbound target are errors that leave the
# variable as it was.
failures <- function() {
  kept <- 1:3
  missing_subscript <- function(at, v) kept[at] <<- v
  missing_double <- function(at, v) kept[[at]] <<- v
  unbound_subscript <- function(v) kept[nowhere] <<- v
  unbound_target <- function(v) absent[1L] <<- v
  local_target <- function(v) { only_local <- 1:3; only_local[1L] <<- v; only_local }
  out <- character(0)
  for (i in 1:4) {
    out <- c(out,
      tryCatch({ missing_subscript(, i); "no error" }, error = function(e) conditionMessage(e)),
      tryCatch({ missing_double(, i); "no error" }, error = function(e) conditionMessage(e)),
      tryCatch({ unbound_subscript(i); "no error" }, error = function(e) conditionMessage(e)),
      tryCatch({ unbound_target(i); "no error" }, error = function(e) conditionMessage(e)),
      tryCatch({ local_target(i); "no error" }, error = function(e) conditionMessage(e)))
  }
  list(out, kept)
}
print(failures())

# The subscript's argument runs once, after the target is read, and may assign the target.
effects <- function() {
  kept <- c(1, 2, 3)
  log <- character(0)
  set <- function(at, v) kept[at] <<- v
  note <- function(at) { log <<- c(log, paste("at", at)); at }
  replace <- function(at) { kept <<- c(10, 20, 30, 40); at }
  remove <- function(at) { rm("kept", envir = parent.frame()); at }
  for (i in 1:4) set(note(2L), i)
  first <- kept
  for (i in 1:4) set(replace(i), -i)
  second <- kept
  out <- character(0)
  for (i in 1:3) {
    kept <- c(7, 8, 9)
    set(remove(i), i)
    out <- c(out, paste(kept, collapse = " "))
  }
  list(first, second, log, out)
}
str(effects())

# A store the subscript's argument makes into the target is replaced by this one, which works
# on the value read before.
nested <- function() {
  kept <- c(1, 2, 3)
  fields <- list(a = 1, b = 2)
  set <- function(at, v) kept[at] <<- v
  set_field <- function(at, v) fields[[at]] <<- v
  store <- function(at) { kept[1L] <<- 100; at }
  store_field <- function(at) { fields[["a"]] <<- 100; at }
  out <- character(0)
  for (i in 1:4) {
    kept <- c(1, 2, 3)
    fields <- list(a = 1, b = 2)
    set(store(2L), i)
    set_field(store_field("b"), i)
    out <- c(out, paste(c(kept, unlist(fields)), collapse = " "))
  }
  out
}
print(nested())

# The target is shared with another variable, an argument of the enclosing function, a global
# and a global the function shadows.
sharing <- function(argument, lazy) {
  kept <- c(1, 2, 3)
  copy <- kept
  change <- function(v) { kept[1L] <<- v; argument[2L] <<- v; lazy[[1L]] <<- v }
  for (i in 1:5) change(i)
  list(kept, copy, argument, lazy)
}
original <- c(5, 6, 7)
str(sharing(original, c(original, 8)))
print(original)
counts <- c(0, 0)
bump <- function(i) counts[i] <<- counts[i] + 1
for (i in 1:6) bump(i %% 2L + 1L)
print(counts)
shadowed <- 1:3
shadow <- function(v) { shadowed <- c(9L, 9L, 9L); shadowed[2L] <<- v; shadowed }
for (i in 4:8) print(shadow(i))
print(shadowed)

# The target is NULL, an environment, a matrix, a named vector that grows, a vector that
# changes its type, and a list that loses an element.
kinds <- function() {
  nothing <- NULL
  registry <- new.env()
  grid <- matrix(1:6, 2L)
  named <- c(a = 1L)
  typed <- 1:3
  items <- list(a = 1, b = 2, c = 3)
  change <- function(i, name) {
    nothing[[i]] <<- name
    registry[[name]] <<- i
    registry$last <<- i
    grid[i] <<- 0L
    named[name] <<- i
    named[["again"]] <<- i
    typed[i] <<- i / 2
    typed[7L] <<- "seven"
    items$b <<- NULL
    items[[name]] <<- NULL
  }
  for (i in 1:3) change(i, letters[i])
  list(nothing, mget(sort(ls(registry)), registry), grid, named, typed, items)
}
str(kinds())

# The value is the target itself.
nest <- function() {
  items <- list(1, 2)
  inside <- function() items[[2L]] <<- items
  for (i in 1:3) inside()
  items
}
str(nest())

# An object without a replacement method keeps its class. One with a method runs it, and the
# method need not force the subscript.
`[<-.tracked` <- function(x, i, value) { calls <<- c(calls, "[<-"); x }
`[[<-.tracked` <- function(x, i, value) { calls <<- c(calls, paste("[[<-", i)); x }
`$<-.tracked` <- function(x, name, value) { calls <<- c(calls, paste("$<-", name)); x }
calls <- character(0)
objects <- function() {
  plain <- structure(1:3, class = "plain")
  tracked <- structure(list(a = 1), class = "tracked")
  frame <- data.frame(a = 1:2)
  stamp <- as.Date("2024-01-01") + 0:2
  log <- character(0)
  note <- function(at) { log <<- c(log, "forced"); at }
  change <- function(at, v) {
    plain[at] <<- v
    tracked[at] <<- v
    tracked[[at]] <<- v
    tracked$a <<- v
    frame$b <<- v
    frame[["c"]] <<- v * 2L
    stamp[2L] <<- as.Date("2025-06-01")
  }
  lazy <- function(at, v) tracked[at] <<- v
  for (i in 1:4) change(2L, i)
  for (i in 1:4) lazy(note(1L), i)
  list(plain, unclass(tracked), frame, stamp, log)
}
str(objects())
print(calls)

# A locked binding and an active binding.
bindings <- function() {
  fixed <- c(1, 2, 3)
  seen <- list()
  makeActiveBinding("live", function(v) if (missing(v)) c(1, 2, 3) else seen[[length(seen) + 1L]] <<- v, environment())
  lockBinding("fixed", environment())
  to_fixed <- function(v) fixed[1L] <<- v
  to_live <- function(at, v) live[at] <<- v
  out <- character(0)
  for (i in 1:4) {
    out <- c(out, tryCatch({ to_fixed(i); "no error" }, error = function(e) conditionMessage(e)))
    to_live(2L, i)
  }
  list(out, fixed, seen)
}
print(bindings())

# Warnings and errors of the replacement.
conditions <- function() {
  kept <- c(1, 2, 3)
  pair <- 1:2
  nothing <- NULL
  recycle <- function(v) kept[pair] <<- v
  coerce <- function(v) kept$a <<- v
  too_many <- function(v) kept[[1L]] <<- v
  out <- character(0)
  for (i in 1:3) {
    kept <- c(1, 2, 3)
    withCallingHandlers({ recycle(1:3); coerce(i) }, warning = function(w) {
      out <<- c(out, conditionMessage(w))
      invokeRestart("muffleWarning")
    })
    out <- c(out, tryCatch({ too_many(1:2); "no error" }, error = function(e) conditionMessage(e)))
  }
  list(out, kept)
}
print(conditions())
