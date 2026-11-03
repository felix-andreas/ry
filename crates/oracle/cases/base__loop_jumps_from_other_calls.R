# GNU R's compiler gives a loop a context only when its body calls eval, evalq or source, or
# has a break or next inside another call, and only such a loop takes a jump from elsewhere.
f <- function() break
r <- tryCatch({ for (i in 1:3) { f(); cat("after", i, "\n") }; "loop done" }, error = function(e) conditionMessage(e))
print(r)
g <- function() eval(quote(break))
r <- tryCatch({ for (i in 1:3) { g(); cat("after", i, "\n") }; "loop done" }, error = function(e) conditionMessage(e))
print(r)
h <- function() { e <- parent.frame(); eval(quote(break), e) }
r <- tryCatch({ for (i in 1:3) { h(); cat("after", i, "\n") }; "loop done" }, error = function(e) conditionMessage(e))
for (i in 1:3) { suppressWarnings(if (i == 2) next); cat("b", i, "\n") }
f <- function() { for (i in 1:3) { eval(quote(if (i == 2) break)); cat("c", i, "\n") }; "c done" }
print(tryCatch(f(), error = function(e) conditionMessage(e)))
f2 <- function() { for (i in 1:3) { evalq(if (i == 2) break); cat("d", i, "\n") }; "d done" }
print(tryCatch(f2(), error = function(e) conditionMessage(e)))
r <- tryCatch({ for (i in 1:3) { eval(quote(if (i == 2) break)); cat("e", i, "\n") }; "e done" }, error = function(e) conditionMessage(e))
print(r)
q <- quote(break)
r <- tryCatch({ for (i in 1:3) { eval(q); cat("q", i, "\n") }; "q done" }, error = function(e) conditionMessage(e)); print(r)
h <- function() eval(quote(break), parent.frame())
r <- tryCatch({ for (i in 1:3) { h(); if (i == 5) break; cat("h", i, "\n") }; "h done" }, error = function(e) conditionMessage(e)); print(r)
r <- tryCatch({ for (i in 1:3) { h(); print(function() break); cat("p", i, "\n") }; "p done" }, error = function(e) conditionMessage(e)); print(r)
w <- function() { for (i in 1:3) { h(); cat("w", i, "\n") }; "w done" }
print(tryCatch(w(), error = function(e) conditionMessage(e)))
w2 <- function() { for (i in 1:3) { h(); if (i == 5) break; cat("w2", i, "\n") }; "w2 done" }
print(tryCatch(w2(), error = function(e) conditionMessage(e)))
w3 <- function() { for (i in 1:3) { h(); g(break); cat("w3", i, "\n") }; "w3 done" }
print(tryCatch(w3(), error = function(e) conditionMessage(e)))
