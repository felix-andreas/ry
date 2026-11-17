# A non-ASCII name written in a call (`c("aä" = 1)`) is marked UTF-8, as a string literal is, so
# looking it up with a literal string matches whichever way the name was made.
m <- c("aä" = "x", b = "y")
print(m["aä"])
print(m[["aä"]])
print(Encoding(names(c("aä" = 1, b = 2))))
print(Encoding(names(list("aä" = 1))))
print(names(m) == "aä")
print(match("aä", names(m)))

l <- list("über" = 1, plain = 2)
print(l$"über")
print(l[["über"]])
print(l["über"])
l[["über"]] <- 10
l["neuß"] <- 3
str(l)

v <- c(1, 2)
names(v) <- c("été", "hiver")
print(v["été"])
v["été"] <- 5
v[["hiver"]] <- 6
print(v)
s <- setNames(1:2, c("å", "b"))
print(s["å"])
print(Encoding(names(s)))

# A name built at run time and a name typed in the source are one name.
built <- paste0("a", "ä")
print(m[built])
e <- new.env()
assign(built, 1, envir = e)
print(get("aä", envir = e))
print(exists("aä", envir = e))
assign("ø", 2, envir = e)
print(sort(ls(e)))
print(mget(c("aä", "ø"), envir = e))
f <- function(...) names(list(...))
print(Encoding(f("ä" = 1, b = 2)))
print(eval(parse(text = "list(`äb` = 1)$`äb`")))
x <- list("ä" = 1)
x$"ä" <- 2
print(x)

# A latin1-marked name matches its UTF-8 spelling, and installs as the same symbol.
n <- iconv("aä", "UTF-8", "latin1")
print(Encoding(n))
x <- c(1, 2); names(x) <- c(n, "b")
print(x["aä"])
print(match("aä", names(x)))
l <- list(1); names(l) <- n
print(l[["aä"]])
print(l$"aä")
e <- new.env(); assign(n, 3, envir = e); print(get("aä", envir = e))
x["aä"] <- 9; print(x)
e <- new.env(); invisible(do.call("$<-", list(e, n, 4))); print(exists("aä", envir = e, inherits = FALSE))
e <- list2env(setNames(list(5), n)); print(get("aä", envir = e))
print(identical(as.name(n), as.name("aä")))

x <- c("ü" = 1)
print(names(x) == "ü")
y <- c(a = 1, "\U0001F600" = 2)
y["\U0001F600"] <- 5
print(y)
tagged <- function(...) {
  v <- c(...)
  v["\U0001F600"] <- 9
  v
}
for (call in 1:3) print(tagged(a = 1, "\U0001F600" = 2))
