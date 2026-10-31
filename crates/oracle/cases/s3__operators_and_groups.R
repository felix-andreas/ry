"+.money" <- function(e1, e2) structure(unclass(e1) + unclass(e2), class = "money")
Ops.temp <- function(e1, e2) {
  v <- get(.Generic)(unclass(e1), unclass(e2))
  if (.Generic %in% c("+", "-", "*", "/")) structure(v, class = "temp") else v
}
a <- structure(1, class = "money")
unclass(a + a)
t1 <- structure(20, class = "temp")
t2 <- structure(30, class = "temp")
unclass(t1 + t2)
t1 < t2
Math.temp <- function(x, ...) structure(get(.Generic)(unclass(x), ...), class = "temp")
unclass(sqrt(structure(16, class = "temp")))
length.stack <- function(x) 99L
length(structure(list(1), class = "stack"))
"[.myvec" <- function(x, i) structure(unclass(x)[i], class = "myvec")
v <- structure(1:5, class = "myvec")
unclass(v[2:3])
"$.rec" <- function(x, name) paste("field", name)
structure(list(), class = "rec")$anything
