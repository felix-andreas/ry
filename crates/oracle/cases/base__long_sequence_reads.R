# A long 1:n is compact, and reading or modifying it behaves like an ordinary vector.
x <- seq_len(2e6)
x[1] <- 0L
x[[2]] <- 5L
print(x[1:4])
print(seq_len(2e6)[1:3])
y <- 1:2e6
z <- y
z[3] <- 9L
print(head(y))
print(head(z))
v <- 1:2e6
acc <- 0L
for (k in 1:2000) acc <- acc + v[k]
print(acc)
print(sum(rev(2e6:1)[1:10]))
# Past the integer range, seq_len is doubles.
print(typeof(seq_len(2^31)))
print(length(seq_len(2^31)))
# attach keeps an active binding active.
e <- new.env()
makeActiveBinding("ab", function() 42, e)
attach(e, name = "abenv")
print(bindingIsActive("ab", as.environment("abenv")))
print(ab)
detach("abenv")
# rep rejects pairlists and returns an empty environment unchanged.
print(identical(rep(emptyenv(), 2), emptyenv()))
tryCatch(rep(pairlist(1, 2), 2), error = function(e) print(conditionMessage(e)))
tryCatch(rep(globalenv(), 2), error = function(e) print(conditionMessage(e)))
invisible(NULL)
