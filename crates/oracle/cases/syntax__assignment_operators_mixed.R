# `=` binds looser than `<-`, `->` and `~`, and tighter than `?`.
a = b <- 5; print(c(a, b))
f <- function() { x = y <- 2; x + y }; print(f())
g <- function(n = m <- 3) n; print(g())
h = function(x) x -> y; print(deparse(h))
d <- function(text) print(deparse(parse(text = text, keep.source = FALSE)[[1]]))
d("a = b <- c"); d("a = b ~ c"); d("a = b ? c"); d("a <- b ? c"); d("a = b -> c")
d("a = b = c <- d = e"); d("a := b <- c"); d("a = b := c"); d("a ~ b = c")
d("a <- b = c"); d("a ? b = c"); d("a -> b <- c"); d("a <- b -> c"); d("a = b -> c = d")
d("if (x) a = b <- c else d = e"); d("function(x) y = z <- 1"); d("for (i in 1:2) a = i <- 3")
d("a = if (x) b <- 1 else c = 2"); d("{a = b <- c; d <- e = f}"); d("(a = b <- c)")
d("f(a = b <- c)"); d("a <<- b = c"); d("a = b <<- c"); d("a = b |> f()"); d("x[a = b <- 1]")
d("a = -b <- 1"); d("a = !b <- 1"); d("a = ~b <- 1")
# `(a <- b) = c` is a replacement call of the function `<-<-`.
print(tryCatch({x = y <- z = 3; "ran"}, error = function(e) conditionMessage(e)))
# The help operator deparses as a call.
print(quote(a ? b)); print(quote(?a)); print(quote(??a)); print(quote(`?`(a, b, c))); print(quote((a = b) ? c))
# An `=` assignment as a call argument deparses in parentheses.
set <- call("=", quote(b), quote(c))
k <- as.call(list(as.name("f"), set, n = set))
print(k); print(k[[2]]); print(quote(x[(a = b)])); print(as.call(list(as.name("["), quote(x), set)))
print(function(a = (b = c)) NULL)
print(as.call(list(as.name("f"), call("<-", quote(b), quote(c)), call("+", set, 1), call("(", set), call("{", set))))
