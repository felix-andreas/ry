Ops.temp <- function(e1, e2) {
  v <- get(.Generic)(unclass(e1), unclass(e2))
  if (.Generic %in% c("+", "-", "*", "/")) structure(v, class = "temp") else v
}
t1 <- structure(20, class = "temp"); t2 <- structure(5, class = "temp")
unclass(t1 + t2)
t1 > t2
class(t1 * 2)
Math.temp <- function(x, ...) { cat("Math:", .Generic, "\n"); structure(NextMethod(), class = "temp") }
unclass(sqrt(structure(16, class = "temp")))
unclass(abs(structure(-3, class = "temp")))
Summary.temp <- function(..., na.rm = FALSE) { cat("Summary:", .Generic, "\n"); NextMethod() }
max(structure(c(1, 9, 3), class = "temp"))
range(structure(c(4, 2), class = "temp"))
"[.myvec" <- function(x, i) structure(unclass(x)[i], class = "myvec")
v <- structure(10:15, class = "myvec")
unclass(v[2:3])
"$.rec" <- function(x, name) paste("field", name)
r <- structure(list(), class = "rec")
r$anything
"==.pt" <- function(e1, e2) "custom equality"
structure(1, class = "pt") == 2
