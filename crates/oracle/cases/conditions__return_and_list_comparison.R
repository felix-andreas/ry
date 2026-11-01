# A list compared with an atomic vector takes the vector's type element by element, `return`
# with no function to return from is an error `tryCatch` catches, and an error message shows
# only the first deparsed line of its call.
p <- list(a = structure("x", class = "json"), b = "null", c = structure("null", class = "json"))
print(p != "null")
print(list(1, "a", TRUE) == "a")
print(list(1L, 2) == c(1, 3))
print(tryCatch(list(1) == list(1), error = conditionMessage))
print(tryCatch(eval(quote(return(1)), globalenv()), error = conditionMessage))
h <- function(x) stop("e")
print(tryCatch(h({ x <- 1; x + 1 }), error = conditionCall))
r <- try(h({ 1 }), silent = TRUE)
cat(r)
f <- function(...) stop("boom")
r <- try(f(aaaaaaaaaaaaaaaaaaaa = 1, bbbbbbbbbbbbbbbbbbbbbbbbbbb = 2, ccccccccccccccccccccc = 3), silent = TRUE)
cat(r)
