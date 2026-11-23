# `object@name` in a function, which runs compiled: each read runs several times, so the first
# read of a site and its later ones are both covered.
suppressMessages(library(methods))
setClass("Holder", representation(
    value = "numeric", label = "character", none = "NULL", symbol = "name",
    call = "call", items = "list", names = "character", fun = "function"
))
holder <- new("Holder",
    value = c(1.5, 2), label = "a", none = NULL, symbol = as.name("sym"),
    call = quote(f(x)), items = list(1, "b"), names = c("p", "q"), fun = function(x) x + 1
)
thrice <- function(f) for (i in 1:3) print(f())

cat("-- slots of each kind\n")
thrice(function() holder@value)
thrice(function() holder@label)
thrice(function() holder@none)
thrice(function() holder@symbol)
thrice(function() holder@call)
thrice(function() holder@items)
thrice(function() holder@names)
thrice(function() holder@fun(1))
thrice(function() holder@"value")
thrice(function() withVisible(holder@value)$visible)

cat("-- the subject is evaluated once, the name is never evaluated\n")
value <- "label"
counted <- function() {
    cat("evaluating the subject\n")
    holder
}
thrice(function() counted()@value)
thrice(function() (function(object) object@value)(holder))

cat("-- the class, the data part and the S3 class\n")
setClass("Counts", contains = "numeric", representation(unit = "character"))
counts <- new("Counts", c(1, 2, 3), unit = "m")
thrice(function() counts@.Data)
thrice(function() counts@unit)
thrice(function() attr(holder@class, "package"))
setClass("Framed", contains = "data.frame", representation(note = "character"))
framed <- new("Framed", data.frame(a = 1:2), note = "n")
thrice(function() framed@.S3Class)
thrice(function() framed@names)
thrice(function() framed@row.names)
thrice(function() framed@note)
setClass("Rows", contains = "data.frame")
rows <- new("Rows", data.frame(a = 1:3))
thrice(function() rows@row.names)
print(slot(rows, "row.names"))

cat("-- a slot the object does not have\n")
reads_absent <- function(object) object@absent
for (i in 1:3) print(tryCatch(reads_absent(holder), error = function(e) {
    c(conditionMessage(e), deparse(conditionCall(e)))
}))
absent_call <- function(expression) tryCatch(expression, error = function(e) deparse(conditionCall(e)))
print(absent_call(holder@absent))
print(absent_call(eval(quote(holder@absent))))
print(absent_call(local(holder@absent)))
print(absent_call(identity(holder@absent)))
print(absent_call(sapply(1, function(i) holder@absent)))

cat("-- a value that is not an S4 object\n")
reads_value <- function(object) object@value
for (subject in list(1, "a", NULL, list(value = 1), quote(name), sum)) {
    for (i in 1:2) print(tryCatch(reads_value(subject), error = function(e) {
        c(conditionMessage(e), deparse(conditionCall(e)))
    }))
}
reads_data <- function(object) object@.Data
for (i in 1:3) print(reads_data(c(1, 2)))

cat("-- an S3 object dispatches a method of `@`\n")
record <- structure(list(value = 10), class = "record")
for (i in 1:2) print(tryCatch(reads_value(record), error = conditionMessage))
`@.record` <- function(object, name) {
    cat("method for", name, "from", deparse(sys.call(sys.parent())), "\n")
    object[[name]]
}
for (i in 1:3) print(reads_value(record))
rm(`@.record`)

cat("-- one site reads S4 objects, S3 objects and plain values in turn\n")
`@.record` <- function(object, name) "from the method"
for (subject in list(holder, record, counts, holder)) {
    print(tryCatch(reads_value(subject), error = conditionMessage))
}
rm(`@.record`)

cat("-- a local `@` is called instead\n")
shadowed <- function(object) {
    `@` <- function(object, name) paste("local", deparse(substitute(name)))
    object@value
}
thrice(function() shadowed(holder))
later <- function(object, define) {
    if (define) assign("@", function(object, name) "assigned later")
    object@value
}
print(later(holder, FALSE))
print(later(holder, TRUE))
print(later(holder, FALSE))

cat("-- a method reads its object's slots\n")
setGeneric("total", function(object, ...) standardGeneric("total"))
setMethod("total", "Holder", function(object, ...) sum(object@value) + length(object@items))
thrice(function() total(holder))
setMethod("total", "Counts", function(object, ...) {
    inner <- function() object@unit
    paste(sum(object@.Data), inner())
})
thrice(function() total(counts))

cat("-- a slot read after the object changes\n")
changing <- holder
for (i in 1:3) {
    changing@value <- i
    print(reads_value(changing))
}
print(reads_value(holder))
