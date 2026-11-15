# A call whose value is its own `...` list: the caller reads the arguments after the frame that
# held the list is gone.
read_dots <- function(dots) {
  holder <- new.env()
  assign("...", dots, envir = holder)
  evalq(list(...), holder)
}
by_get <- function(...) get("...")
print(deparse(read_dots(by_get(1 + 1, "a"))))
by_environment <- function(...) environment()$...
print(deparse(read_dots(by_environment(2 * 3, b = "c"))))
kept <- by_get(sum(1:4), "later")
invisible(gc())
print(deparse(read_dots(kept)))
forwarded <- function(...) by_get(...)
print(deparse(read_dots(forwarded(10L, 20L))))
