make_counter <- function() { count <- 0; function() { count <<- count + 1; count } }
a <- make_counter(); b <- make_counter()
a(); a(); b(); a()
adders <- lapply(1:3, function(i) function(x) x + i)
sapply(adders, function(f) f(10))
bank <- function(balance = 0) {
  list(deposit = function(x) { balance <<- balance + x; invisible(balance) },
       get = function() balance)
}
acct <- bank(100); acct$deposit(50); acct$get()
x <- 1
f <- function() { x <- 2; g <- function() x; g() }
f()
h <- function() x
k <- function() { x <- 3; h() }
k()
environmentName(environment(h))
