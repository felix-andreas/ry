e <- new.env()
assign("v", 10, envir = e)
get("v", envir = e); exists("v", envir = e); exists("v", envir = e, inherits = FALSE)
exists("nope"); mget(c("v"), envir = e)
local({ a <- 2; b <- 3; a * b })
f <- function() { x <- "local"; g <- function() parent.frame(); identical(g(), environment()) }
f()
counter_env <- new.env(); counter_env$n <- 0
bump <- function() counter_env$n <- counter_env$n + 1
for (i in 1:3) bump(); counter_env$n
sort(ls(e)); rm("v", envir = e); ls(e)
outer_fn <- function() { inner_var <- 5; inner_fn <- function() inner_var * 2; inner_fn() }
outer_fn()
h <- function(...) sys.call(); h(1, b = 2)
k <- function(n) if (n == 0) sys.nframe() else k(n - 1); k(3) - k(0)
environmentName(globalenv()); environmentName(baseenv()); environmentName(emptyenv())
is.environment(environment(sum)); identical(parent.env(globalenv()), globalenv())
eval(quote(x * 2), list(x = 21)); evalq(y, list(y = "q")); with(list(a = 1, b = 2), a + b)
