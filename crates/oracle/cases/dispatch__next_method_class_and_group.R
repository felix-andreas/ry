# NextMethod passes .Class from the class whose method it found, and always defines .Group.
foo <- function(x) UseMethod("foo"); x <- structure(1, class=c("a","b","c")); foo.a <- function(x) {print(.Class); NextMethod()}; foo.b <- function(x) {print(.Class); NextMethod()}; foo.c<-function(x) {print(.Class); print(attr(.Class,"previous")); NextMethod()}; foo.default<-function(x) {print(.Class); print(exists(".Group",inherits=FALSE)); print(.Group)}; foo(x)
bar <- function(x) UseMethod("bar"); bar.a <- function(x) NextMethod(); bar.c <- function(x) .Class; print(bar(x))
.Group <- "GLOBAL"
baz <- function(x) UseMethod("baz"); baz.a <- function(x) { .Group <- ""; NextMethod() }; baz.b <- function(x) .Group; print(baz(x))
