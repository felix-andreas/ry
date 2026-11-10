# Growing a list with `[[<-` past its end hands the elements to the grown list. Elements shared
# with other variables keep their values, a list shared with another variable is copied, and a
# growth that fails leaves the list as it was.
acc <- list(); for (i in 1:3000) acc[[i]] <- c(i, i); print(length(acc)); print(acc[[2999]])
inner <- list(1, "a"); outer <- list(inner, inner)
outer[[5]] <- 5; inner[[1]] <- 0; str(outer[[1]]); str(inner)
original <- list(p = 1, q = list(2)); alias <- original
alias[[4]] <- "new"; str(original); str(alias)
named <- list(a = 1); named[["c"]] <- 3; named[[5]] <- 5; str(named)
f <- function(l) { l[[10]] <- "f"; l }; base <- list(1, 2); out <- f(base); print(length(base)); print(length(out))
keep <- list(1, 2)
result <- tryCatch({ keep[[4]] <- stop("no value"); "assigned" }, error = conditionMessage)
print(result); str(keep)
nested <- list(); for (i in 1:50) nested[[i]] <- list(i); nested[[60]] <- nested[[1]]
print(length(nested)); print(nested[[60]][[1]]); print(is.null(nested[[55]]))
ex <- expression(a + 1); ex[[3]] <- quote(b); print(ex)
gc_sum <- 0; for (round in 1:5) { big <- list(); for (i in 1:20000) big[[i]] <- i; gc_sum <- gc_sum + sum(unlist(big)) }
print(gc_sum)
