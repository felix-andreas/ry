# break and next outside a loop are errors, including in a function called from a loop; a switch
# that falls through to only empty alternatives returns invisible NULL; TRE reads a stray ) literally.
r <- tryCatch(eval(quote({ if (TRUE) next; 1 })), error = function(e) conditionMessage(e)); print(r)
f <- function() next
r <- tryCatch(f(), error = function(e) conditionMessage(e)); print(r)
r <- tryCatch(eval(compiler::compile(quote({ if (TRUE) next; 1 }))), error = function(e) conditionMessage(e)); print(r)
r <- tryCatch(local(break), error = function(e) conditionMessage(e)); print(r)
for (i in 1:2) { g <- function() break; r <- tryCatch(g(), error = function(e) conditionMessage(e)); print(r) }
f <- function(type) switch(type, response = { 1 }, link = , terms = )
print(withVisible(f("link")))
print(withVisible(f("terms")))
print(withVisible(f("zzz")))
g <- function(type) { x <- 5; switch(type, response = { x <- 1 }, link = , terms = ); x }
print(g("link"))
print(gsub("a)", "X", "a)b"))
