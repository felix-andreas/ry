# Every dispatch below goes through one call site, so its inline cache sees each change.
describe <- function(x) UseMethod("describe")
describe.default <- function(x) "default"
site <- function(x) describe(x)
obj <- structure(1, class = c("child", "parent"))

# A method defined after the first call, redefined, replaced by a non-function, and removed.
print(site(obj))
describe.parent <- function(x) "parent"
print(site(obj))
describe.child <- function(x) paste("child, then", NextMethod())
print(site(obj))
describe.child <- function(x) "child v2"
print(site(obj))
describe.child <- "not a function"
print(site(obj))
rm(describe.child)
print(site(obj))
rm(describe.parent)
print(site(obj))

# A polymorphic site: more classes than one entry holds, in turn and repeated.
for (class in c("a", "b", "c", "d", "e", "a", "e")) {
  assign(paste0("describe.", class), local({ k <- class; function(x) k }))
}
print(vapply(c("a", "b", "c", "d", "e", "a", "e", "zz"), function(class) {
  site(structure(1, class = class))
}, ""))
rm(describe.c)
print(vapply(c("a", "c", "e"), function(class) site(structure(1, class = class)), ""))

# Implicit classes: a matrix, an integer dispatching to numeric, a function.
describe.matrix <- function(x) "matrix"
describe.numeric <- function(x) "numeric"
describe.integer <- function(x) "integer"
describe.function <- function(x) "function"
print(c(site(matrix(1:4, 2)), site(1L), site(1.5), site(sum)))
rm(describe.integer)
print(c(site(1L), site(1.5)))
rm(describe.matrix)
print(site(matrix(1:4, 2)))

# A method registered in a namespace, then shadowed by a global definition.
registerS3method("describe", "registered", function(x) "registered", envir = environment(describe))
reg <- structure(1, class = "registered")
print(site(reg))
describe.registered <- function(x) "global wins"
print(site(reg))
rm(describe.registered)
print(site(reg))

# A method in a calling function's frame is seen there and not after.
caller <- function(x) {
  describe.parent <- function(x) "frame method"
  site(x)
}
print(c(caller(obj), site(obj), caller(obj)))

# NextMethod down an inherits chain whose middle method appears and disappears.
chain <- structure(1, class = c("x1", "x2", "x3"))
describe.x1 <- function(x) paste("x1 >", NextMethod())
describe.x3 <- function(x) "x3"
print(site(chain))
describe.x2 <- function(x) paste("x2 >", NextMethod())
print(site(chain))
rm(describe.x2)
print(site(chain))
for (i in 1:3) print(site(chain))

# Group generics: an Ops method defined after the first use, then a specific one over it.
money <- function(v) structure(v, class = "money")
add <- function(a, b) a + b
print(unclass(add(money(1), money(2))))
Ops.money <- function(e1, e2) paste("Ops", .Generic)
print(add(money(1), money(2)))
`+.money` <- function(e1, e2) "plus method"
print(add(money(1), money(2)))
print(money(1) * money(2))
rm(`+.money`)
print(add(money(1), money(2)))
rm(Ops.money)
print(unclass(add(money(1), money(2))))

# A class attribute changed on the same object between calls.
changing <- structure(1, class = "a")
out <- character()
for (class in c("a", "b", "a", "d")) {
  class(changing) <- class
  out <- c(out, site(changing))
}
print(out)
