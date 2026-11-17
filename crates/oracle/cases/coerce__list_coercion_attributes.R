# An atomic vector that becomes a list keeps its names and no other attribute, whichever
# operation coerces it; `storage.mode<-` and `mode<-` keep them all.
classed <- function() structure(1:3, names = c("a", "b", "c"), extra = "e", class = "zz")
grid <- function() structure(1:4, dim = c(2L, 2L), dimnames = list(c("r1", "r2"), c("c1", "c2")), extra = "e")
listed <- function() structure(list(1L, 2L), names = c("a", "b"), extra = "e", class = "zz")
show <- function(label, expr) {
    cat(label, ": ")
    dput(withCallingHandlers(expr, warning = function(w) {
        cat("(warning:", conditionMessage(w), ") ")
        invokeRestart("muffleWarning")
    }))
}
show("as.vector list", as.vector(classed(), "list"))
show("as.vector list, matrix", as.vector(grid(), "list"))
show("as.list", as.list(unclass(classed())))
show("as.list, matrix", as.list(grid()))
show("as.list, one-dimensional array", as.list(array(1:2, 2L, list(c("p", "q")))))
show("[<- list", { x <- classed(); x[2] <- list("B"); x })
show("[<- list, matrix", { x <- grid(); x[2] <- list("B"); x })
show("[<- list, matrix cell", { x <- grid(); x[1, 2] <- list("B"); x })
show("[<- list, named cell", { x <- grid(); x["r2", "c2"] <- list(1i); x })
show("[<- list, matrix row", { x <- grid(); x[1, ] <- list("r", "s"); x })
show("[<- list, index matrix", { x <- grid(); x[cbind(2, 1)] <- list("m"); x })
show("[<- list, array cell", { x <- array(1:8, c(2, 2, 2)); x[1, 2, 2] <- list("q"); x })
show("[[<- list", { x <- unclass(classed()); x[[2]] <- list("B"); x })
show("$<-", { x <- classed(); x$b <- "B"; x })
show("$<-, matrix", { x <- grid(); x$b <- "B"; x })
show("$<-, factor", { x <- factor(c("a", "b")); x$new <- -1L; x })
show("$<-, date", { x <- as.Date("2024-01-02"); x$y <- 1; x })
show("$<- NULL", { x <- structure(c(1.5, 2), names = c("a", "b"), extra = "e"); x$c <- NULL; x })
show("storage.mode<-", { x <- classed(); storage.mode(x) <- "list"; x })
show("storage.mode<-, matrix", { x <- grid(); storage.mode(x) <- "list"; x })
show("mode<-", { x <- classed(); mode(x) <- "list"; x })
show("as.expression", as.expression(classed()))
show("as.vector expression", as.vector(classed(), "expression"))
show("as.vector pairlist", as.vector(unclass(classed()), "pairlist"))
show("c", c(classed(), list(1)))
show("append", append(grid(), list(1)))
show("lapply", lapply(grid(), identity))

# A list that becomes atomic keeps its names too, and a list and an expression vector
# exchange every attribute.
show("list to integer", as.vector(listed(), "integer"))
show("as.integer", as.integer(listed()))
show("storage.mode<-, list", { x <- listed(); storage.mode(x) <- "integer"; x })
show("storage.mode<-, list matrix", { x <- structure(list(1L, 2L, 3L, 4L), dim = c(2L, 2L), extra = "e"); storage.mode(x) <- "integer"; x })
show("unlist", unlist(listed()))
show("list to expression", as.vector(unclass(listed()), "expression"))
show("expression to list", as.vector(structure(expression(a = 1, b = x), extra = "e"), "list"))
show("list [<- atomic", { x <- list(1, 2); x[1:3] <- classed(); x })

# Between atomic types every attribute follows.
show("[<- character", { x <- classed(); x[2] <- "B"; x })
show("[[<- character", { x <- structure(1:2, extra = "e", names = c("p", "q")); x[["k"]] <- "x"; x })
show("storage.mode<- double", { x <- grid(); storage.mode(x) <- "double"; x })

# `[[<-` with subscripts for each extent reads the dims before a list value makes the object a
# list.
show("[[<- matrix cell", { x <- matrix(1:4, 2); x[[1, 2]] <- list(1); x })
show("[[<- matrix cell by name", { x <- grid(); x[["r1", "c2"]] <- list("v"); x })
show("[[<- array cell", { x <- array(1:8, c(2, 2, 2)); x[[1, 2, 2]] <- list("z"); x })
show("[[<- matrix position", { x <- matrix(1:4, 2); x[[3]] <- list(1); x })
