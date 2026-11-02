# Out-of-bounds subscripts signal subscriptOutOfBoundsError with the object, dimension and subscript.
sh <- function(expr) { e <- tryCatch(expr, error = function(e) e); if (!inherits(e, "error")) { cat("ok\n"); return(invisible()) }; cat(class(e)[1], deparse(conditionCall(e)), "|", conditionMessage(e), "|"); if (!is.null(e$object)) cat(deparse(e$object), typeof(e$subscript), e$subscript, typeof(e$index), deparse(e$index)); cat("\n") }
x <- 1:3; l <- list(a = 1, b = 2); m <- matrix(1:4, 2, dimnames = list(c("r1","r2"), NULL))
sh(x[[5]]); sh(x[["a"]]); sh(l[["c"]]); sh(l[[5]]); sh(l[[c(1, 5)]]); sh(l[[c("a","z")]])
sh(m[3, 1]); sh(m[1, 3]); sh(m["r3", 1]); sh(m[[3, 1]]); sh(m[[1, 5]]); sh(m[[5]])
sh(x[[-1]]); sh(x[[2.5e9]]); sh(l[[NA_character_]]); sh(x[[NA_integer_]])
sh(x[5, 1]); f <- function() { y <- list(a=1); y[["b"]][["c"]] <- 1 }; sh(f())
sh(m[[3, 1]] <- 1); sh(m[3, 1] <- 1); sh(x[[0]]); sh(as.pairlist(l)[[5]]); sh(quote(f(a))[[5]])
e <- new.env(); sh(e[[1]]); sh(array(1:8, c(2,2,2))[1, 1, 3])
m <- matrix(1:4, 2, dimnames = list(c("r1","r2"), NULL)); x <- 1:3; l <- list(a=1)
sh(m[c(1,3), 1]); sh(m[c("r1","r3"), 1]); sh(m[1, c(TRUE,TRUE,TRUE)]); sh(m[cbind(3,1)])
sh(m[c(1,3), 1] <- 0); sh(x[[5]] <- 1:2); sh(x[[NaN]] <- 1); sh(l[[Inf]] <- 1); sh(m[["r3", 1]]); sh(m[[NA, 1]])
sh(l[[c(1,2,3)]]); sh(list(1, list(2))[[c(2, 1, 1)]]); sh(list(1, list(2))[[c(2, 5)]])
f <- function() { l <- list(a = list(b = 1)); l[["a"]][["c"]][["d"]] }; sh(f())
