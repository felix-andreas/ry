foo_obj <- structure(1, class = "foo"); bar_obj <- structure(1, class = "bar")
`+.foo` <- function(e1, e2) "foo"; `+.bar` <- function(e1, e2) "bar"
chooseOpsMethod.foo <- function(x, y, mx, my, cl, reverse) { cat("foo: x=", class(x), "y=", class(y), "rev=", reverse, "mx is +.foo:", identical(mx, `+.foo`), "cl=", deparse(cl), "\n"); FALSE }
chooseOpsMethod.bar <- function(x, y, mx, my, cl, reverse) { cat("bar: x=", class(x), "y=", class(y), "rev=", reverse, "mx is +.bar:", identical(mx, `+.bar`), "\n"); TRUE }
print(foo_obj + bar_obj)
print(bar_obj + foo_obj)
`%*%.testit` <- function(x, y) "mm"
x <- structure(1:4, class="testit")
print(x %*% 1:4); print(`%*%`(x))
print(1:4 %*% x)
