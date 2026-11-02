# dirname edge cases, cumulative functions over lists, substitute(...()) and substitute(value),
# match.call through forwarded dots, names<- on non-vectors, paste on a classed string, and lengths
# dispatching length on classed elements.
print(dirname(c("", "a", "/a", "a//b", "//a", "/", "a/b/", "//")))
print(cumprod(list(3L, 2L)))
print(cumsum(list(1, 2.5)))
f <- function(...) substitute(...())
x <- f(a, b + 1, c = d)
print(typeof(x)); print(x)
g <- function(...) substitute(list(...))
print(typeof(g(a, b)))
h <- function(...) substitute(...())
print(h())
`f<-` <- function(x, value) { print(substitute(value)); x }
v <- 1
f(v) <- 3:4 + 1L
g <- function(x, ...) { Call <- match.call(expand.dots = TRUE); print(Call); Call[[1]] <- quote(list); eval.parent(Call) }
forward <- function(...) g(...)
outer_call <- function(value) forward(value + 1, 2)
print(outer_call(1))
for (x in list(function() 1, as.name("a"), new.env(), quote(f(1)))) {
    print(tryCatch({ names(x) <- "a"; "no error" }, error = function(e) conditionMessage(e)))
}
as.character.bar <- function(x, ...) "B"
print(paste0(structure("a", class = "bar")))
print(paste0(structure(1, class = "bar")))
print(sprintf("%s", structure("a", class = "bar")))
length.qq <- function(x) 99L
print(lengths(list(structure(1, class = "qq"), 1:3)))
