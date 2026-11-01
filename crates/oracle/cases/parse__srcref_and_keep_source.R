print(getOption("keep.source"))
plain <- function(x) x  # not kept
print(attributes(plain))
e <- parse(text = c("x <- 1 # c", "f <- function(a,  b) {", "  a + b  # sum", "}", "é <- 2; y <- 3"), keep.source = TRUE)
print(names(attributes(e)))
sr <- attr(e, "srcref")
print(length(sr))
for (s in sr) print(unclass(s)[1:8])
print(sr[[2]])
print(as.character(sr[[2]]))
print(class(attr(e, "srcfile")))
print(attr(e, "srcfile")$lines)
print(unclass(attr(e, "wholeSrcref")))
eval(e)
print(names(attributes(f)))
f
print(f, useSource = FALSE)
print(names(attributes(body(f))))
print(lapply(attr(body(f), "srcref"), unclass))
print(unclass(attr(body(f), "wholeSrcref")))
print(e[[2]][[3]][[4]])
deparse(f)
deparse(f, control = "useSource")
print(unclass(getSrcref(f)))
getSrcLines(attr(e, "srcfile"), 2, 3)
e <- parse(text = c("x  +  1", "function(a)   a"), keep.source = TRUE)
e
deparse(e, control = "useSource")
deparse(e[[2]], control = "useSource")
e <- parse(text = "\tx <- \t1\n\t\ty", keep.source = TRUE)
print(lapply(attr(e, "srcref"), unclass))
e <- parse(text = "\"a\nb\" ;  `x\ny`", keep.source = TRUE)
print(lapply(attr(e, "srcref"), unclass))
e <- parse(text = c("a", "b"), keep.source = TRUE, srcfile = NULL)
print(attributes(e))
e <- parse(text = c("x |> f()", "\\(x) x", "a -> b", "-1", "(function(y) y)(2)"), keep.source = TRUE)
print(lapply(attr(e, "srcref"), unclass))
e <- parse(text = c("a", "b", "c"), n = 2, keep.source = TRUE)
print(length(attr(e, "srcref")))
options(keep.source = TRUE)
e2 <- parse(text = "h <- function()  NULL")
print(names(attributes(e2)))
k <- function(z)   z*2
k
outer <- function() {
  inner()
}
inner <- function() {
  lapply(sys.calls(), function(call) unclass(attr(call, "srcref")))
}
res <- outer()
print(length(res))
print(res[[2]])
compiled <- function(n) {
  acc <- 0
  for (i in seq_len(n)) {
    acc <- acc + i
  }
  where()
}
where <- function() sys.call()
for (round in 1:3) w <- compiled(2)
print(unclass(attr(w, "srcref")))
tb <- function() {
  .Internal(traceback(0))
}
calls <- tb()
print(calls[[1]])
print(unclass(attr(calls[[1]], "srcref")))
print(interactive())
