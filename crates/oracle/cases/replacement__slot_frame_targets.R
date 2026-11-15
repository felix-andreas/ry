# Replacement calls and element assignments inside functions whose variables are not plain
# locals: an argument nobody has read yet, a variable of the enclosing function or the global
# environment, an object with and without a replacement method, an environment, and a
# replacement function written in R. Each runs several times so compiled code runs it too, and
# a method defined between two runs must be found by the next one.
run <- function(f, ...) {
  for (k in 1:4) f(...)
  f(...)
}

# An argument that the replacement is the first to read, evaluated once.
unread <- function(x) {
  names(x) <- c("a", "b")
  x
}
print(run(unread, { cat("argument\n"); c(1, 2) }))
unread_element <- function(x, i, v) {
  x[[i]] <- v
  x
}
print(run(unread_element, { cat("x\n"); c(1, 2, 3) }, { cat("i\n"); 2 }, { cat("v\n"); 9 }))
unread_matrix <- function(m, i, j) {
  m[i, j] <- 0
  m
}
print(run(unread_matrix, { cat("m\n"); matrix(1, 2, 2) }, { cat("i\n"); 1 }, { cat("j\n"); 2 }))
caller_keeps <- function() {
  original <- c(a = 1, b = 2)
  changed <- unread(original)
  list(original, changed)
}
print(run(caller_keeps))
missing_index <- function(x, i) {
  x[[i]] <- 0
  x
}
print(tryCatch(run(missing_index, 1:3), error = function(e) conditionMessage(e)))

# A variable of the enclosing function: the assignment makes a local copy.
enclosing <- function() {
  v <- c(1, 2, 3)
  m <- matrix(0, 2, 2)
  inner <- function() {
    v[2] <- 9
    names(v) <- c("a", "b", "c")
    attr(v, "k") <- "local"
    m[2, 1] <- 5
    list(v, m)
  }
  list(inner(), v, m)
}
print(run(enclosing))
shared <- c(x = 1, y = 2)
global_target <- function() {
  shared[["x"]] <- 100
  names(shared)[2] <- "z"
  dim(shared) <- c(1L, 2L)
  shared
}
print(run(global_target))
print(shared)
unbound_target <- function() {
  nowhere[1] <- 1
}
print(tryCatch(run(unbound_target), error = function(e) conditionMessage(e)))
unbound_replacement <- function() {
  names(nowhere) <- "a"
}
print(tryCatch(run(unbound_replacement), error = function(e) conditionMessage(e)))

# An object without a replacement method uses the builtin's own code and keeps its class.
record <- structure(list(a = 1), class = "record")
plain_object <- function(s) {
  s$a <- 2
  s[["b"]] <- 3
  s[3] <- list("c")
  names(s)[3] <- "third"
  attr(s, "note") <- "kept"
  s
}
str(run(plain_object, record))
str(record)

# With a method, the method runs, wherever it is defined.
`$<-.metered` <- function(x, name, value) {
  cat("$<-.metered", name, "\n")
  y <- unclass(x)
  y[[name]] <- value
  structure(y, class = "metered")
}
`[[<-.metered` <- function(x, i, value) {
  cat("[[<-.metered", i, "\n")
  y <- unclass(x)
  y[[i]] <- value
  structure(y, class = "metered")
}
`[<-.metered` <- function(x, i, value) {
  cat("[<-.metered", i, "\n")
  y <- unclass(x)
  y[i] <- value
  structure(y, class = "metered")
}
`names<-.metered` <- function(x, value) {
  cat("names<-.metered\n")
  attr(x, "names") <- toupper(value)
  x
}
`dim<-.metered` <- function(x, value) {
  cat("dim<-.metered\n")
  x
}
`levels<-.metered` <- function(x, value) {
  cat("levels<-.metered\n")
  x
}
with_methods <- function(s) {
  s$a <- 2
  s[["b"]] <- 3
  s[1] <- list(0)
  names(s) <- c("p", "q")
  dim(s) <- 2L
  levels(s) <- "l"
  s
}
str(run(with_methods, structure(list(a = 1), class = "metered")))

local_method <- function() {
  `names<-.nearby` <- function(x, value) {
    cat("local names<-.nearby\n")
    attr(x, "names") <- rev(value)
    x
  }
  `[<-.nearby` <- function(x, i, value) {
    cat("local [<-.nearby\n")
    unclass(x)
  }
  s <- structure(list(1, 2), class = "nearby")
  names(s) <- c("a", "b")
  s[1] <- list(5)
  s
}
str(run(local_method))
enclosing_method <- function() {
  `$<-.outer` <- function(x, name, value) {
    cat("enclosing $<-.outer\n")
    x
  }
  inner <- function(s) {
    s$a <- 1
    s
  }
  inner(structure(list(), class = "outer"))
}
str(run(enclosing_method))

# A method that appears after the function has run is found by the next call.
late <- function(s) {
  names(s) <- c("a", "b")
  s$a <- 10
  s
}
late_object <- structure(list(1, 2), class = "late")
str(run(late, late_object))
`names<-.late` <- function(x, value) {
  cat("names<-.late\n")
  attr(x, "names") <- paste0(value, "!")
  x
}
`$<-.late` <- function(x, name, value) {
  cat("$<-.late\n")
  x
}
str(run(late, late_object))
rm(`names<-.late`, `$<-.late`)
str(run(late, late_object))

# Inherited and default methods.
`names<-.base_kind` <- function(x, value) {
  cat("names<-.base_kind\n")
  attr(x, "names") <- value
  x
}
inherits_method <- function() {
  s <- structure(list(1), class = c("derived_kind", "base_kind"))
  names(s) <- "n"
  s
}
str(run(inherits_method))
implicit_class_method <- function() {
  m <- structure(matrix(1:4, 2), class = "tagged")
  dimnames(m) <- list(c("a", "b"), NULL)
  f <- factor(c("u", "v", "u"))
  levels(f) <- c("p", "q")
  f[2] <- "p"
  d <- data.frame(a = 1:2, b = 3:4)
  names(d) <- c("u", "v")
  d$u <- d$u * 2L
  d[["w"]] <- c("x", "y")
  d[1, "v"] <- 0L
  dimnames(d) <- list(c("r1", "r2"), c("A", "B", "C"))
  class(d) <- c("extra", class(d))
  attr(d, "note") <- "n"
  oldClass(d) <- "data.frame"
  list(m, f, d)
}
print(run(implicit_class_method))

# Environments change where they are, with or without a class.
environment_target <- function(e) {
  e$count <- (if (is.null(e$count)) 0 else e$count) + 1
  e[["label"]] <- "set"
  e
}
env <- new.env()
invisible(run(environment_target, env))
print(mget(c("count", "label"), envir = env))
classed <- structure(new.env(), class = "holder")
invisible(run(environment_target, classed))
print(c(classed$count, class(classed)))
state <- new.env()
state$total <- 0
state$add <- function(n) {
  state$total <- state$total + n
  invisible(exists("state", inherits = FALSE))
}
for (k in 1:5) made_local <- state$add(k)
print(c(state$total, made_local))
`$<-.guarded` <- function(x, name, value) {
  cat("$<-.guarded", name, "\n")
  assign(name, value, envir = x)
  x
}
guarded <- structure(new.env(), class = "guarded")
set_guarded <- function() {
  guarded$a <- 1
  guarded[["b"]] <- 2
  sort(ls(guarded))
}
print(run(set_guarded))
locked <- new.env()
locked$fixed <- 1
lockBinding("fixed", locked)
set_locked <- function() {
  locked$fixed <- 2
}
print(tryCatch(run(set_locked), error = function(e) conditionMessage(e)))
active <- new.env()
makeActiveBinding("live", function(v) if (missing(v)) "read" else cat("active set", v, "\n"), active)
set_active <- function() {
  active$live <- 3
  active[["live"]] <- 4
  active$live
}
print(run(set_active))

# S4 objects: slots, and replacement methods set with setReplaceMethod and setMethod.
setClass("Account", representation(balance = "numeric", tags = "character"))
slots <- function(a) {
  slot(a, "balance") <- 10
  a@tags <- c("x", "y")
  attr(a, "note") <- "s4"
  a
}
account <- new("Account", balance = 1, tags = character())
print(run(slots, account))
print(account@balance)
bracket <- function(a) {
  a[1] <- 5
  a$extra <- "e"
  a[["deep"]] <- 1
  names(a) <- "renamed"
  a
}
print(tryCatch(run(bracket, account), error = function(e) "no method yet"))
setMethod("[<-", "Account", function(x, i, ..., value) {
  cat("S4 [<-\n")
  x@balance[i] <- value
  x
})
setMethod("$<-", "Account", function(x, name, value) {
  cat("S4 $<-", name, "\n")
  x@tags <- c(x@tags, name)
  x
})
setMethod("[[<-", "Account", function(x, i, ..., value) {
  cat("S4 [[<-", i, "\n")
  x
})
setMethod("names<-", "Account", function(x, value) {
  cat("S4 names<-\n")
  x@tags <- value
  x
})
print(run(bracket, account))

# Replacement functions written in R.
`second<-` <- function(x, value) {
  x[2] <- value
  x
}
`element<-` <- function(x, i, scale = 1, value) {
  x[i] <- value * scale
  x
}
closure_setters <- function(arg) {
  local <- c(1, 2, 3)
  keep <- local
  second(local) <- 20
  element(local, 3) <- 30
  element(local, 1, scale = 2) <- 5
  element(local, i = 2L, 10) <- 1
  second(arg) <- -1
  list(local, keep, arg)
}
print(run(closure_setters, c(7, 8, 9)))
`inspect<-` <- function(x, value) {
  cat("call:", deparse(sys.call()), "\n")
  cat("matched:", deparse(match.call()), "\n")
  cat("caller has marker:", get("marker", envir = parent.frame()), "\n")
  cat("frames:", sys.nframe(), "function:", deparse(sys.call(-1)[[1]]), "\n")
  cat("value missing:", missing(value), "\n")
  x + value
}
inspected <- function() {
  marker <- "yes"
  v <- 1
  inspect(v) <- 2
  v
}
print(run(inspected))
`failing<-` <- function(x, value) stop("refused: ", value)
`warning_setter<-` <- function(x, value) {
  warning("careful")
  value
}
setter_conditions <- function() {
  v <- 1
  first <- tryCatch({ failing(v) <- 5; "unreached" }, error = function(e) {
    paste(conditionMessage(e), "|", deparse(conditionCall(e)))
  })
  second <- withCallingHandlers(
    { warning_setter(v) <- 6; v },
    warning = function(w) {
      cat("warned:", conditionMessage(w), "\n")
      invokeRestart("muffleWarning")
    }
  )
  list(first, second, v)
}
print(run(setter_conditions))
`returning<-` <- function(x, value) {
  on.exit(cat("setter exits\n"))
  if (value > 0) return(value * 2)
  x
}
early_return <- function() {
  v <- 1
  returning(v) <- 4
  w <- 3
  returning(w) <- -1
  c(v, w)
}
print(run(early_return))
`nonlocal<-` <- function(x, value) c(x, value)
counter <- 0
nonlocal_closure_target <- function() {
  nonlocal(counter) <- 1
  counter
}
print(run(nonlocal_closure_target))
print(counter)
lazy_default <- function() {
  `pad<-` <- function(x, width = length(x) + 1L, value) {
    length(x) <- width
    x[width] <- value
    x
  }
  v <- 1:2
  pad(v) <- 9L
  pad(v, 5L) <- 7L
  v
}
print(run(lazy_default))

# Matrix cells.
matrix_cells <- function(arg) {
  owned <- matrix(0, 2, 2)
  owned[1, 2] <- 3
  alias <- owned
  owned[2, 2] <- 4
  arg[1, 1] <- 100
  int <- matrix(1:4, 2)
  int[2, 1] <- 0.5
  chr <- matrix(letters[1:4], 2)
  chr[1, 1] <- "z"
  lgl <- matrix(TRUE, 2, 2)
  lgl[2, 2] <- NA
  list(owned, alias, arg, int, chr, lgl)
}
caller_matrix <- matrix(1, 2, 2)
print(run(matrix_cells, caller_matrix))
print(caller_matrix)
matrix_errors <- function() {
  m <- matrix(0, 2, 2)
  v <- 1:4
  list(
    tryCatch({ m[3, 1] <- 1; "grew" }, error = function(e) conditionMessage(e)),
    tryCatch({ v[1, 1] <- 1L; "assigned" }, error = function(e) conditionMessage(e)),
    tryCatch({ m[0, 1] <- 1; dim(m) }, error = function(e) conditionMessage(e)),
    { m[-1, 2] <- 7; m },
    { m[TRUE, 1] <- 8; m }
  )
}
print(run(matrix_errors))
