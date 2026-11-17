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

# Pattern: small S3 objects with operator methods and methods that call NextMethod().
new_term <- function(v, k) structure(list(v = v, k = k), class = c("term", "expr"))
"+.expr" <- function(e1, e2) new_term(e1$v + e2$v, e1$k)
"[.expr" <- function(x, i) structure(NextMethod(), class = class(x))
length.expr <- function(x) 1L
format.expr <- function(x, ...) paste0(x$v, x$k)
describe <- function(x, ...) UseMethod("describe")
describe.default <- function(x, ...) "value"
describe.expr <- function(x, ...) "expr"
describe.term <- function(x, ...) paste("term", NextMethod())

a <- new_term(1, "a")
b <- new_term(2, "a")

bench("s3: UseMethod() to a class method", function(i) describe(b))
bench("s3: UseMethod() to the default method", function(i) describe(i))
bench("s3: method that calls NextMethod()", function(i) describe(a))
bench("s3: operator method a + b", function(i) a + b)
bench("s3: `[` method with NextMethod()", function(i) a["v"])
bench("s3: length() method", function(i) length(a))
bench("s3: format() method", function(i) format(a))
bench("s3: x$v on a classed list", function(i) a$v)
bench("s3: structure() of a classed list", function(i) new_term(i, "a"))
bench("s3: inherits(x, 'expr')", function(i) inherits(a, "expr"))
bench("s3: class(x)[1] and oldClass<-", function(i) { x <- a; oldClass(x) <- "expr"; class(x)[1] })
bench("s3: print() of a classed object to a string", function(i) capture.output(print(unclass(a))))
