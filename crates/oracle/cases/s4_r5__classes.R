setClass("Person", representation(name = "character", age = "numeric"))
setGeneric("greet", function(obj, ...) standardGeneric("greet"))
setMethod("greet", "Person", function(obj, ...) cat("Hello,", obj@name, "\n"))
p <- new("Person", name = "Ada", age = 36)
greet(p)
isVirtualClass("Person")
is(p, "Person")
p@age <- 37
p@age
setClass("Student", contains = "Person", representation(school = "character"))
s <- new("Student", name = "Bo", age = 20, school = "MIT")
greet(s)
is(s, "Person")
setMethod("show", "Person", function(object) cat("<Person ", object@name, ">\n", sep = ""))
p
s
tryCatch(new("Person", name = 1), error = function(e) "invalid slot type")
setValidity("Person", function(object) if (object@age < 0) "age must be >= 0" else TRUE)
tryCatch(new("Person", name = "X", age = -1), error = function(e) conditionMessage(e))
Account <- setRefClass("Account", fields = list(balance = "numeric"),
  methods = list(deposit = function(x) { balance <<- balance + x; invisible(.self) }))
a <- Account$new(balance = 10)
a$deposit(5)
a$balance
