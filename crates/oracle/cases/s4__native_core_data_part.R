coerce_table <- environment(getGeneric("coerce<-"))$.MTable
setClass("Num", contains = "numeric", representation(u = "character"))
n1 <- new("Num", c(1, 2), u = "a")
n2 <- new("Num", c(3, 4), u = "b")
attributes(n2); typeof(n2); n2@.Data
new("Num", 1:3, u = "c")@.Data
new("Num", .Data = c(5, 6), u = "d")
new("Num", u = "e", .Data = c(7, 8))
new("Num", n1, u = "f")
validObject(n2)
tryCatch(new("Num", "x"), error = function(e) conditionMessage(e))
sort(grep("^Num", names(coerce_table), value = TRUE))

setClass("Fn", contains = "function", representation(tag = "character"))
f1 <- new("Fn", function(a) a + 1, tag = "t")
f2 <- new("Fn", function(a) a * 2, tag = "u")
f2(4); f2@tag; is(f2, "function"); validObject(f2)
names(attributes(f2))

setClass("Lst", contains = "list", representation(n = "integer"))
l1 <- new("Lst", list(1, "a"), n = 2L)
l2 <- new("Lst", list(x = 1), n = 1L)
attributes(l2); length(l2)
validObject(l2)

m1 <- new("MethodDefinition", function(x) x)
m2 <- new("MethodDefinition", function(x, y) x + y)
attributes(m2)
m2(1, 2)
validObject(m2)
setGeneric("area", function(shape, ...) standardGeneric("area"))
setClass("Sq", representation(side = "numeric"))
setMethod("area", "Sq", function(shape, ...) shape@side^2)
method <- getMethod("area", "Sq")
class(method); names(attributes(method)); method@target; method@defined; method@generic
area(new("Sq", side = 3))
sort(grep("Fn|Lst|Method", names(coerce_table), value = TRUE))
