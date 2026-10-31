setClass("Temp", representation(celsius = "numeric"))
setClass("TempF", representation(fahrenheit = "numeric"))
setAs("Temp", "TempF", function(from) new("TempF", fahrenheit = from@celsius * 9 / 5 + 32))
as(new("Temp", celsius = 100), "TempF")@fahrenheit
as(3L, "character")
as(3.7, "integer")
as("1", "numeric")
setClass("Base", representation(id = "integer"))
setClass("Derived", contains = "Base", representation(label = "character"))
x <- new("Derived", id = 3L, label = "z")
class(as(x, "Base"))
is(x, "Base")
is(1, "numeric")
is(x)
methods::is(matrix(1), "matrix")
Account <- suppressMessages(setRefClass("Account",
  fields = list(owner = "character", balance = "numeric"),
  methods = list(
    deposit = function(x) {
      balance <<- balance + x
      invisible(.self)
    },
    withdraw = function(x) {
      if (x > balance) stop("insufficient funds")
      balance <<- balance - x
      invisible(.self)
    },
    show = function() cat("Account of", owner, ":", balance, "\n")
  )))
a <- Account$new(owner = "Ann", balance = 100)
a$deposit(50)
a$balance
a$withdraw(30)$deposit(5)
a$balance
a
tryCatch(a$withdraw(1000), error = function(e) conditionMessage(e))
b <- a
b$deposit(1)
a$balance
copied <- a$copy()
copied$deposit(1000)
c(a$balance, copied$balance)
Savings <- suppressMessages(setRefClass("Savings", contains = "Account", fields = list(rate = "numeric"),
  methods = list(
    addInterest = function() deposit(balance * rate),
    withdraw = function(x) {
      cat("Savings withdraw\n")
      callSuper(x)
    }
  )))
s <- Savings$new(owner = "Sam", balance = 200, rate = 0.1)
s$addInterest()
s$balance
s$withdraw(20)
s$balance
is(s, "Account")
class(s)
a$balance <- 7
a$balance
Counter <- suppressMessages(setRefClass("Counter", fields = list(count = "numeric"),
  methods = list(
    initialize = function(...) {
      count <<- 0
      callSuper(...)
    },
    inc = function() {
      count <<- count + 1
      invisible(.self)
    }
  )))
counter <- Counter$new()
counter$inc()$inc()
counter$count
Person <- suppressMessages(setRefClass("Person", fields = list(name = "character")))
Person$new(name = "Zed")
Person$fields()
