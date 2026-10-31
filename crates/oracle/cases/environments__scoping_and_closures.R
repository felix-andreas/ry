make_counter <- function() {
  count <- 0
  function() {
    count <<- count + 1
    count
  }
}
counter <- make_counter()
counter(); counter()
other <- make_counter()
other()
x <- "global"
f <- function() { x <- "local"; g <- function() x; g() }
f()
exists("count", envir = environment(counter))
get("count", envir = environment(counter))
