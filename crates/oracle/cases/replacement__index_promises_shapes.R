# A replacement function written in R receives the target's other arguments as promises, so a
# subscript it never uses runs zero times, one it forces twice runs once, and substitute(),
# missing(), nargs() and match.call() see the code. Each shape runs three times inside a
# function (the body compiles after its first call), in a loop, at top level and under replicate.
trace <- character()
note <- function(tag, value = 1) { trace <<- c(trace, tag); value }
show <- function(label, f) {
  for (n in 1:3) {
    trace <<- character()
    out <- tryCatch(paste(deparse(f()), collapse = " "), error = function(e) paste("Error:", conditionMessage(e)))
    cat(sprintf("%s #%d: %s | trace: %s\n", label, n, out, paste(trace, collapse = ",")))
  }
}

`[<-.lz` <- function(x, i, j, value) {
  attr(x, "seen") <- paste(deparse(substitute(i)), if (missing(j)) "<missing j>" else deparse(substitute(j)), nargs(), missing(i))
  x
}
`[[<-.lz` <- function(x, i, value) { attr(x, "seen") <- paste(deparse(substitute(i)), nargs()); x }
`$<-.lz` <- function(x, name, value) { attr(x, "seen") <- paste(deparse(substitute(name)), name); x }
`lz<-` <- function(x, i, value) { attr(x, "seen") <- deparse(substitute(i)); x }
`twice<-` <- function(x, i, value) { i; i; attr(x, "seen") <- i; x }
`mc<-` <- function(x, i, value) { attr(x, "seen") <- deparse(match.call()); x }
`ix<-` <- function(x, i, j, value) { attr(x, "seen") <- c(deparse(substitute(i)), deparse(substitute(j))); x }
z0 <- structure(list(), class = "lz")
setClass("Hold", representation(v = "ANY"))

show("S3 [<- lazy expr", function() { z <- z0; z[note("i") == 1, note("j")] <- 5; attr(z, "seen") })
show("S3 [<- symbol", function() { z <- z0; k <- note("k"); z[k] <- 5; attr(z, "seen") })
show("S3 [<- missing i", function() { z <- z0; z[, note("j")] <- 5; attr(z, "seen") })
show("S3 [<- three subscripts", function() { z <- z0; z[note("i"), note("j"), note("k")] <- 1; attr(z, "seen") })
show("S3 [<- in a loop", function() { z <- z0; for (k in 1:3) z[note("i")] <- k; attr(z, "seen") })
show("S3 [<- loop variable", function() { z <- z0; for (k in 1:3) z[k] <- k; attr(z, "seen") })
show("S3 [<- formal", function() { g <- function(z, k) { z[k] <- 1; attr(z, "seen") }; g(z0, note("k")) })
show("S3 [<- missing formal", function() { g <- function(z, k) { z[k] <- 1; attr(z, "seen") }; g(z0) })
show("S3 [[<-", function() { z <- z0; z[[note("i")]] <- 5; attr(z, "seen") })
show("S3 $<-", function() { z <- z0; z$abc <- 5; attr(z, "seen") })
show("closure f<- unforced", function() { z <- z0; lz(z, note("i")) <- 5; attr(z, "seen") })
show("closure f<- forced twice", function() { z <- z0; twice(z, note("i", 7)) <- 5; attr(z, "seen") })
show("closure f<- match.call", function() { z <- z0; mc(z, note("i")) <- 5; attr(z, "seen") })
show("closure f<- two subscripts", function() { z <- z0; ix(z, note("i"), note("j")) <- 1; attr(z, "seen") })
show("closure f<- local", function() { z <- z0; k <- 3; lz(z, k) <- 1; attr(z, "seen") })
show("closure f<- formal", function() { g <- function(z, k) { lz(z, k) <- 1; attr(z, "seen") }; g(z0, note("k")) })
show("nested f(g(x), i)", function() { z <- list(a = z0); lz(z$a, note("i")) <- 5; attr(z$a, "seen") })
show("nested x$a[i]", function() { z <- list(a = z0); z$a[note("i")] <- 1; attr(z$a, "seen") })
show("nested x[[1]][i]", function() { z <- list(z0); z[[1]][note("i")] <- 1; attr(z[[1]], "seen") })
show("nested attr(x)[i]", function() { z <- 1; attr(z, "a") <- z0; attr(z, "a")[note("i")] <- 5; attr(attr(z, "a"), "seen") })
show("nested S4 slot [i]", function() { h <- new("Hold", v = z0); h@v[note("i")] <- 1; attr(h@v, "seen") })
show("nested S4 slot f<-", function() { h <- new("Hold", v = z0); lz(h@v, note("i")) <- 1; attr(h@v, "seen") })
show("superassign S3", function() { z <- z0; g <- function() z[note("i")] <<- 5; g(); attr(z, "seen") })
show("replicate", function() { z <- z0; replicate(2, { z[notDefined == 1] <- 5; 1 }) })

# Builtin replacements get values, in the order GNU R evaluates them: the value, then the
# subscripts left to right, each once.
show("names(x)[i]", function() { z <- c(a = 1, b = 2); names(z)[note("i", 2)] <- "q"; names(z) })
show("levels(x)[i]", function() { f <- factor(c("a", "b")); levels(f)[note("i", 2)] <- "q"; levels(f) })
show("vector [<- order", function() { z <- 1:3; z[note("i", 2)] <- note("v", 9L); z })
show("matrix [<- order", function() { z <- matrix(1:4, 2); z[note("i", 1), note("j", 2)] <- note("v", 9L); z })
show("attr<- order", function() { z <- 1:3; attr(z, note("n", "q")) <- note("v", 9L); attributes(z) })
show("substr<- order", function() { s <- "abcdef"; substr(s, note("a", 2), note("b", 3)) <- "XY"; s })
show("plain loop", function() { z <- numeric(3); for (k in 1:3) z[k] <- k; z })
show("data frame", function() { d <- data.frame(a = 1:2); d[d$a == 1, "a"] <- 5L; d$a })

for (n in 1:3) {
  trace <- character()
  z <- z0
  z[note("i") == 1, note("j")] <- 5
  lz(z, note("i")) <- 5
  cat(sprintf("top level #%d: %s | trace: %s\n", n, attr(z, "seen"), paste(trace, collapse = ",")))
}
print(replicate(3, { z <- z0; z[notDefined == 1, "a"] <- 5; lz(z, notDefined) <- 5; attr(z, "seen") }))
