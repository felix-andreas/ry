# `object@name <- value` in a function, which runs compiled, and at top level. Each write runs
# several times, so the first write of a site and its later ones are both covered.
suppressMessages(library(methods))
setClass("Base", representation("VIRTUAL"))
setClass("Leaf", contains = "Base", representation(size = "numeric"))
setOldClass(c("stamp", "POSIXct"))
setClassUnion("Text", c("character", "NULL"))
setClass("Holder", representation(
    value = "numeric", label = "character", any = "ANY", part = "Base", items = "list",
    when = "stamp", text = "Text", fun = "function", flag = "logical", count = "integer"
))
stamp <- structure(0, class = c("stamp", "POSIXct"))
holder <- new("Holder", value = 1, label = "a", part = new("Leaf", size = 1), when = stamp)
leaf <- new("Leaf", size = 2)
outcome <- function(expression) {
    tryCatch(expression, error = function(e) c(conditionMessage(e), deparse(conditionCall(e))[1]))
}
show_slot <- function(object, name) str(attr(object, name))

cat("-- a value of the slot's class\n")
set_value <- function(object, value) {
    object@value <- value
    object
}
for (i in 1:3) show_slot(set_value(holder, i + 0.5), "value")
set_label <- function(object, value) {
    object@label <- value
    object
}
for (i in 1:3) show_slot(set_label(holder, letters[i]), "label")
show_slot(holder, "value")

cat("-- a slot of class ANY takes any value\n")
set_any <- function(object, value) {
    object@any <- value
    object
}
for (value in list(1L, "text", NULL, sum, quote(name), leaf, list(1, 2))) {
    show_slot(set_any(holder, value), "any")
}

cat("-- a value of a class that extends the slot's class\n")
for (i in 1:3) show_slot(set_value(holder, i), "value")
set_part <- function(object, value) {
    object@part <- value
    object
}
for (i in 1:3) print(is(set_part(holder, leaf)@part, "Leaf"))
set_text <- function(object, value) {
    object@text <- value
    object
}
for (value in list("words", NULL, "more")) show_slot(set_text(holder, value), "text")
set_when <- function(object, value) {
    object@when <- value
    object
}
for (i in 1:3) print(class(set_when(holder, stamp)@when))

cat("-- a value of a class the slot does not take\n")
for (i in 1:3) print(outcome(set_value(holder, "text")))
print(outcome(set_label(holder, 1)))
print(outcome(set_part(holder, holder)))
print(outcome(set_text(holder, 1L)))
print(outcome(set_when(holder, structure(0, class = c("other", "stamp")))))
set_flag <- function(object, value) {
    object@flag <- value
    object
}
print(outcome(set_flag(holder, NA_character_)))
show_slot(set_flag(holder, NA), "flag")
set_count <- function(object, value) {
    object@count <- value
    object
}
print(outcome(set_count(holder, 1)))
show_slot(set_count(holder, 1L), "count")

cat("-- values with dimensions and S3 classes, by the slot's class\n")
setClass("Shapes", representation(
    numbers = "numeric", grid = "matrix", cube = "array", items = "vector", whole = "integer",
    level = "factor", frame = "data.frame", shaped = "structure", listed = "list"
))
shapes <- new("Shapes")
writer <- function(name) {
    eval(substitute(function(object, value) {
        object@NAME <- value
        "accepted"
    }, list(NAME = as.name(name))))
}
values <- list(
    matrix = matrix(1:4, 2), array = array(1:8, c(2, 2, 2)), double = 1.5, integer = 2L,
    factor = factor("a"), ordered = factor("a", ordered = TRUE), frame = data.frame(a = 1),
    list = list(1), null = NULL
)
for (name in slotNames("Shapes")) {
    write <- writer(name)
    taken <- vapply(values, function(value) {
        identical(outcome(write(shapes, value)), "accepted") &&
            identical(outcome(write(shapes, value)), "accepted")
    }, NA)
    cat(name, "takes", names(values)[taken], "\n")
}

cat("-- a slot the class does not have\n")
set_absent <- function(object, value) {
    object@absent <- value
    object
}
for (i in 1:3) print(outcome(set_absent(holder, 1)))
print(outcome({
    holder@absent <- 1
}))

cat("-- an object that is not an S4 object\n")
for (subject in list(1, "a", list(value = 1))) print(outcome(set_value(subject, 1)))

cat("-- one site writes values of several classes in turn\n")
for (value in list(1, 2L, "text", 3, TRUE, 4)) print(outcome(attr(set_value(holder, value), "value")))

cat("-- the caller's object is unchanged, a local one changes in place\n")
grow <- function(object, times) {
    for (i in seq_len(times)) object@value <- object@value + i
    object@value
}
for (i in 1:3) print(grow(holder, i))
show_slot(holder, "value")
changing <- holder
for (i in 1:3) {
    changing@value <- i * 10
    changing@label <- paste("label", i)
    print(c(changing@value, holder@value))
    print(changing@label)
}

cat("-- a write to a slot of a slot\n")
nest <- function(object, size) {
    object@part@size <- size
    object
}
for (i in 1:3) print(nest(holder, i)@part@size)
print(outcome(nest(holder, "big")))

cat("-- `slot<-` and `@<-` called by name\n")
by_name <- function(object, name, value) {
    slot(object, name) <- value
    object
}
for (i in 1:3) show_slot(by_name(holder, "value", i + 0.25), "value")
print(outcome(by_name(holder, "value", "text")))
show_slot(`@<-`(holder, "label", "called"), "label")
show_slot(`@<-`(holder, label, "by symbol"), "label")
print(outcome(`@<-`(holder, "label", 2)))

cat("-- a replacement method writes its object's slot\n")
setGeneric("size<-", function(object, value) standardGeneric("size<-"))
setReplaceMethod("size", "Leaf", function(object, value) {
    object@size <- value
    object
})
for (i in 1:3) {
    size(leaf) <- i
    print(leaf@size)
}
print(outcome({
    size(leaf) <- "wide"
}))

cat("-- a class defined after the first write to a site\n")
setClass("Wide", contains = "Leaf", representation(width = "numeric"))
wide <- new("Wide", size = 1, width = 2)
for (i in 1:2) print(is(set_part(holder, wide)@part, "Wide"))
setClass("Holder", representation(value = "character", label = "character"))
redefined <- new("Holder", value = "now text", label = "b")
for (i in 1:3) show_slot(set_value(redefined, paste("text", i)), "value")
print(outcome(set_value(redefined, 1)))

# `@<-` finds `checkAtAssignment` once, at the first slot write of the session.
cat("-- a check replaced after the first write is not called\n")
namespace <- asNamespace("methods")
original <- get("checkAtAssignment", namespace)
unlockBinding("checkAtAssignment", namespace)
assign("checkAtAssignment", function(cl, name, valueClass) {
    cat("checking", cl, name, valueClass, "\n")
    original(cl, name, valueClass)
}, namespace)
for (i in 1:2) show_slot(set_label(redefined, "checked"), "label")
print(outcome(set_label(redefined, 1)))
assign("checkAtAssignment", original, namespace)
lockBinding("checkAtAssignment", namespace)
for (i in 1:2) show_slot(set_label(redefined, "unchecked"), "label")
