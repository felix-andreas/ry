# A replacement closure sees `*tmp*` in its call and can evaluate it, reports its own call in
# errors and warnings, and "not subsettable" errors are classed conditions naming the type.
show <- function(e) print(e)
`f<-` <- function(x, value) stop("bad")
z <- 1
tryCatch(f(z) <- 1, error = function(e) print(conditionCall(e)))
`w<-` <- function(x, value) { warning("careful"); x }
withCallingHandlers(w(z) <- 1, warning = function(w) {
  print(conditionCall(w))
  invokeRestart("muffleWarning")
})
`g<-` <- function(x, value) { print(sys.call()); print(match.call()); x }
g(z) <- 2
`h<-` <- function(x, value) { print(eval(sys.call()[[2]], parent.frame())); x }
h(z) <- 3
exists("*tmp*")
x <- list(); x[[quote(a)]] <- 1; str(x)
y <- c(a = 1); y[[quote(a)]] <- 5; y
n <- list(1); names(n) <- NA; n[[NA_character_]] <- 2; str(n)
tryCatch({ l <- list(e = new.env()); l[[c("e", "v")]] <- 1 }, error = show)
q <- list(); tryCatch(q[[1e20]] <- 1, error = show)
fn <- function() 1
tryCatch(sum[1], error = show)
tryCatch(`if`[1], error = show)
tryCatch(sum[[1]], error = show)
tryCatch(quote(a)[1], error = show)
tryCatch(quote(a)$b, error = show)
tryCatch(fn[1], error = show)
tryCatch(fn$a, error = show)
tryCatch(sum$a, error = show)
tryCatch({ v <- 1:2; v$a }, error = show)
tryCatch({ v <- sum; v[1] <- 2 }, error = show)
tryCatch({ v <- sum; v[[1]] <- 2 }, error = show)
tryCatch({ v <- quote(a); v$b <- 2 }, error = show)
tryCatch({ v <- fn; v$a <- 1 }, error = show)
tryCatch({ v <- new.env(); v[1] <- 1 }, error = show)
e <- tryCatch(new.env()[1], error = identity)
class(e)
is.environment(e$object)
