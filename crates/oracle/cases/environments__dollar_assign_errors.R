# `e$name <- value` on an environment reports the assignment as the error's call, whether the
# loop has warmed up or not.
f <- function() { g <- emptyenv(); g$y <- 2 }
for (i in 1:3) tryCatch(f(), error = function(c) print(conditionCall(c)))
h <- function(e, i) e$a <- i
locked <- new.env(); locked$a <- 1; lockBinding("a", locked)
for (i in 1:3) tryCatch(h(locked, i), error = function(c) print(conditionMessage(c)))
frozen <- new.env(); lockEnvironment(frozen)
for (i in 1:3) tryCatch(h(frozen, i), error = function(c) print(conditionMessage(c)))
open <- new.env()
for (i in 1:5) h(open, i)
print(open$a)
