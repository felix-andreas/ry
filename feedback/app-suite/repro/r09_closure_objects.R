# bench(label, f) times f(i). It doubles the number of calls until one batch takes 0.3 s of CPU
# time, then prints the label and the nanoseconds per call of the last batch. With the environment
# variable UNDER_HANDLER=1, the calls run inside withCallingHandlers(), as the code of a test does.
under_handler <- nzchar(Sys.getenv("UNDER_HANDLER"))
run <- function(n, f) for (i in seq_len(n)) f(i)
bench <- function(label, f) {
  n <- 64
  repeat {
    start <- sum(proc.time()[1:2])
    if (under_handler) withCallingHandlers(run(n, f), warning = function(w) NULL) else run(n, f)
    seconds <- sum(proc.time()[1:2]) - start
    if (seconds >= 0.3) break
    n <- n * 2
  }
  cat(sprintf("%s\t%.0f\n", label, 1e9 * seconds / n))
}

# Pattern: an object is a list of closures that share one environment and change its variables
# with `<<-`. The fastmap package builds its maps this way, and a model builder creates one map
# for every term of a model.
library(fastmap)

counter <- function() {
  n <- 0
  keys <- character()
  values <- list()
  add <- function(key, value) {
    n <<- n + 1
    keys[n] <<- key
    values[[n]] <<- value
    invisible(TRUE)
  }
  get <- function(key) values[[match(key, keys)]]
  size <- function() n
  list(add = add, get = get, size = size)
}

m <- fastmap()
m$set("a", 1)
obj <- counter()
obj$add("a", 1)

bench("closures: build an object of 3 closures", function(i) counter())
bench("closures: method that assigns with <<-", function(i) { o <- counter(); o$add("a", i); o$add("b", i) })
bench("closures: method that reads", function(i) obj$get("a"))
bench("closures: fastmap()", function(i) fastmap())
bench("closures: fastmap set() of one key", function(i) m$set("a", i))
bench("closures: fastmap get() of one key", function(i) m$get("a"))
bench("closures: fastmap new map, 3 sets, as_list()", function(i) { x <- fastmap(); x$set("a", 1); x$set("b", 2); x$set("d", 3); x$as_list() })
bench("closures: environment assign() and get()", function(i) { e <- new.env(); assign("a", i, envir = e); get("a", envir = e) })
bench("closures: local() block", function(i) local({ a <- i; a + 1 }))

# The same methods on objects that hold many elements. A write with `<<-` to one element of a
# long vector must not copy the vector.
big <- fastmap()
for (k in seq_len(10000)) big$set(paste0("k", k), k)
long <- counter()
for (k in seq_len(10000)) long$add(paste0("k", k), k)
store <- function() {
  values <- numeric(10000)
  list(put = function(i, v) { values[i] <<- v; invisible(TRUE) }, at = function(i) values[i])
}
st <- store()
bench("closures: fastmap set() of an old key, 10,000 keys", function(i) big$set("k500", i))
bench("closures: fastmap set() of a new key, 10,000 keys", function(i) big$set(paste0("n", i), i))
bench("closures: fastmap get(), 10,000 keys", function(i) big$get("k500"))
bench("closures: fastmap has(), 10,000 keys", function(i) big$has("k501"))
bench("closures: values[i] <<- v on 10,000 elements", function(i) st$put(500L, i))
bench("closures: values[[n]] <<- v appends to 10,000", function(i) long$add("z", i))
bench("closures: new fastmap, 200 sets, as_list()", function(i) { x <- fastmap(); for (k in 1:200) x$set(paste0("k", k), k); x$as_list() })
