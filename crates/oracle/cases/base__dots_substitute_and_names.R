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

# A builtin called with a forwarded missing argument falls back to its default.
forward_signif <- function(x, digits) signif(x, digits = digits)
print(forward_signif(1.23456789))
forward_round <- function(x, digits) round(x, digits = digits)
print(forward_round(1.56789))

# A vector assigned into itself reads the old values.
permuted <- c(10, 20, 30)
permuted[c(3L, 1L, 2L)] <- permuted
print(permuted)
swap_ints <- function(x) { x[c(2L, 1L)] <- x; x }
print(swap_ints(c(5L, 6L)))
swapped_list <- list(1, 2)
swapped_list[2:1] <- swapped_list
str(swapped_list)

# A default forced inside a method that `[` dispatched to still sees its own call.
frame_data <- data.frame(a = 1:3)
by_count <- function(x, i, drop = (nargs() >= 3L)) frame_data[, 1, drop = drop]
print(by_count(1, 2))
by_call <- function(x, i, drop = sys.call()) frame_data[, 1, drop = { print(drop); FALSE }]
invisible(by_call(1, 2))
