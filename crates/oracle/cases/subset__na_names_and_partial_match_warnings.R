# `$` reads an NA name as the text "NA"; `[[` matches an NA name with nothing.
l <- list(1, 2, 3); names(l) <- c(NA, "b", "NA")
print(l$N)
print(l$`NA`)
print(l[["NA"]])
print(l[[NA_character_]])
print(l[["N", exact = FALSE]])
k <- list(1, 2); names(k) <- c("NAME", NA)
print(k$N)
print(k$NA_)

# `[[<-` matches the text "NA" against an NA name, and an NA subscript against both.
w <- l; w[["NA"]] <- 30; str(w)
w <- l; w[[NA_character_]] <- 40; str(w)
w <- list(a = 1); w[[NA_character_]] <- 2; print(names(w)); w[[NA_character_]] <- 3; str(w)
w <- l; w$`NA` <- 50; str(w)
v <- c(a = 1, 2); names(v)[2] <- NA; v[["NA"]] <- 9; print(v)

# `exact = NA` warns about each partial match, against the `[[` call.
f <- function(x) x[["a", exact = NA]]
show <- function(expr) withCallingHandlers(expr, warning = function(w) {
  cat("warning in", deparse(conditionCall(w)), ":", conditionMessage(w), "\n")
  invokeRestart("muffleWarning")
})
print(show(f(list(abc = 1))))
print(show(f(list(abc = 1, abd = 2))))
print(show(f(list(abc = 1, abd = 2, abe = 3))))
print(show(f(list(a = 1, ab = 2))))
print(show(list(abc = 1)[["a", exact = FALSE]]))
print(show(list(abc = 1)[["a"]]))
print(show(c(abc = 1)[["a", exact = NA]]))
print(tryCatch(c(abc = 1, abd = 2)[["a", exact = NA]], warning = function(w) "warned", error = function(e) conditionMessage(e)))
print(show(list(x = list(abc = 1))[[c("x", "a"), exact = NA]]))

# `$<-` interns its name: an empty name is an error, and `NA` is the name "NA".
l <- list(a = 1)
print(tryCatch(`$<-`(l, "", 2), error = function(e) conditionMessage(e)))
str(`$<-`(l, NA_character_, 2))
l2 <- list(1, 2); names(l2) <- c("a", NA)
str(`$<-`(l2, NA_character_, 3))
