# A Summary group method always receives `na.rm`, last, and its call holds the evaluated arguments,
# as GNU R's `fixup_NaRm` arranges.
Summary.foo <- function(..., na.rm) { print(sys.call()); print(na.rm); 1 }
x <- structure(1, class = "foo")
max(x, 2)
sum(x, na.rm = TRUE)
range(x)
o <- factor(c("a","b"), ordered = TRUE); print(max(o)); print(range(o, o))
sum(na.rm = TRUE, x)
