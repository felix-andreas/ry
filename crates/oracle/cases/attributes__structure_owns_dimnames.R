# structure() gives its result a dimnames list of its own, as GNU R's `attributes<-` does, so C
# code that writes into the result's list (xts's `dimnames<-` method clears the row names that
# way) cannot change the object structure() was called on.
same_object <- function(a, b) {
    address <- function(v) sub(" .*", "", capture.output(.Internal(inspect(v)))[1])
    identical(address(a), address(b))
}
m <- matrix(1:4, 2, dimnames = list(c("a", "b"), c("x", "y")))
s <- structure(m, extra = 1)
same_object(attr(s, "dimnames"), attr(m, "dimnames"))
str(attributes(s))
s <- structure(m, class = "tagged")
same_object(attr(s, "dimnames"), attr(m, "dimnames"))
# An object nothing else holds is modified in place, and its dimnames list is still copied.
local({
    x <- matrix(1:2, 1, dimnames = list("r", c("p", "q")))
    before <- attr(x, "dimnames")
    y <- structure(x, extra = 2)
    print(same_object(attr(y, "dimnames"), before))
    str(attributes(y))
})
# Names and other attribute values are shared, as in GNU R.
v <- c(a = 1, b = 2)
same_object(names(structure(v, extra = 1)), names(v))
