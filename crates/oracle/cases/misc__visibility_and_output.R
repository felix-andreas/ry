quiet <- function() return(invisible(1))
quiet()
withVisible(quiet())$visible
none <- function() {
  if (TRUE) return(invisible())
  2
}
none()
capture.output(print(1), cat("two\n"), invisible(3))
collected <- textConnection("lines", "w", local = FALSE)
sink(collected)
print("diverted")
sink()
close(collected)
lines
cat(c("aaaa", "bbbb", "cccc", "dd"), sep = ", ", fill = 12)
cat(c("aaaa", "bbbb", "cccc", "dd"), sep = " ", fill = 9, labels = c("a:", "b:"))
cat(1:30, fill = 20)
x <- pairlist(a = 1, b = 2)
typeof(x[1])
typeof(x[integer(0)])
x[2] <- list(5)
typeof(x)
length(c(pairlist(a = 1, b = 2), 3))
quote(f(a))[integer(0)]
print(alist(x = ))
list(a = structure(1, foo = "bar"))
inner <- function(where) missing(where)
outer <- function(where = 1) inner(where)
outer()
