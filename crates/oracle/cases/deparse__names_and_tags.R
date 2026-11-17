# Names that are not syntactic, as deparse, dput and print write them: element names and
# attribute names raw between quotes, call tags and symbols escaped, in backticks under
# `backtick` and in double quotes otherwise.
nm <- c("a", "a b", "a\\b", "a\nb", "a\"b", "a'b", "a`b", "if", "", NA, "é", "a\tb", "é x", "1a", ".1", "..1", "...", "_a", "a.b", "TRUE", "NULL", "a\u0001b", "\\", "`", "a\rb", "a\\nb", "Inf", "function", "a$b", "\U0001F600")
show <- function(x) for (ctl in list("keepNA", "all", NULL, c("niceNames", "showAttributes"), "exact")) for (bt in c(TRUE, FALSE)) tryCatch(cat(deparse(x, control = ctl, backtick = bt), sep = "\n"), error = function(e) cat("E:", conditionMessage(e), "\n"))
v <- seq_along(nm); names(v) <- nm; show(v)
l <- as.list(v); show(l)
ch <- as.character(v); names(ch) <- nm; show(ch)
cl <- as.call(c(list(as.name("f")), l)); show(cl)
pl <- as.pairlist(l); show(pl)
fm <- as.function(c(l[nm != "" & !is.na(nm)], quote(x))); show(fm); print(fm)
show(as.call(list(as.name("function"), as.pairlist(l[nm != "" & !is.na(nm)]), quote(x))))
show(structure(1, names = "a\nb", foo = 2, `a b` = 3, `a\\b` = 4, `a\nb` = 5, `if` = 6))
e <- as.expression(l); show(e)
for (n in nm[!is.na(nm) & nm != ""]) { show(as.name(n)); show(call(n, 1)); show(as.call(list(as.name("$"), quote(x), as.name(n)))); show(as.call(list(as.name("$"), quote(x), n))); show(as.call(list(as.name("::"), as.name(n), as.name(n)))); show(as.call(list(as.name("<-"), as.name(n), 1))); show(as.call(list(as.name("@"), quote(x), as.name(n)))) }
print(v); print(l[1:6]); str(l[1:6]); dput(v); dput(l); dput(pl); dput(cl)
df <- data.frame(1, 2); names(df) <- c("a\\b", "a\nb"); dput(df)
x <- list(1); attr(x, "a\\b") <- 1; dput(x)
print(quote(f(`a\\b` = 1, `a\nb` = 2))); print(quote(list(`a b` = 1)$`a b`))

ok <- nm[!is.na(nm) & nm != ""]
v2 <- seq_along(ok); names(v2) <- ok; show(v2); show(as.list(v2)); show(as.character(v2) |> setNames(ok)); show(as.pairlist(as.list(v2))); show(as.call(c(list(as.name("f")), as.list(v2)))); dput(v2); print(deparse(v2))
show(structure(list(1), names = "a\nb", class = "foo")); show(structure(1:2, names = c("a b", "c"), `a\nb` = 1)); show(pairlist(1, 2)); show(as.pairlist(alist(a = , `b c` = 1, 2))); show(pairlist(a = quote(x + y), b = pairlist(c = 1)))

# Printing: a list element name is escaped as an unquoted string, so a backtick in it stays.
nm <- c("a", "a b", "a\\b", "a\nb", "a`b", "é", "a\tb", "é x", "if", "a\001b", "\U0001F600", "a\"b", NA, "")
for (n in nm) { print(as.name(if (is.na(n) || n == "") "x" else n)); l <- list(1); names(l) <- n; print(l); print(list(k = l)); print(pairlist(l)) }
l <- setNames(list(1, 2), c("a\\b", "c`d")); str(l); print(as.pairlist(l)); print(as.call(c(quote(f), l)))
e <- quote(f(x)); e[[1]] <- as.name("a\nb"); print(e)
print(tryCatch(print(call("function", 1)), error = function(e) conditionMessage(e)))
f <- function(`a b`, `c\\d` = 1) NULL; print(f); print(formals(f)); print(args(f)); print(deparse(f, backtick = FALSE))
print(quote(`a b`$`c d`)); print(quote(x$`a\\b`)); print(quote(`my var` + 1)); print(quote(`a\nb`(1)))
print(body(function() `a b` <- `c\\d`[`e f`]))
