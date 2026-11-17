# A call of a keyword or operator is written in its form whatever arguments it holds, with
# NULL for each one it lacks, and a function expression whose formals are no list is an error.
show <- function(e) tryCatch(cat(deparse(e), sep = "\n"), error = function(e) cat("E:", conditionMessage(e), "\n"))
for (name in c("if", "while", "for", "repeat", "(", "{", "function", "break", "next", "return", "[", "[[", "$", "@", "<-", "=", "+", "!", "~", "?", ":", "::", "%in%", "&&", "-", "^", "quote", "c")) {
  cat("==", name, "\n")
  show(as.call(list(as.name(name))))
  show(call(name, 1))
  show(call(name, quote(a), quote(b)))
  show(call(name, quote(a), quote(b), quote(c)))
  show(call(name, quote(a), quote(b), quote(c), quote(d)))
  show(call(name, quote(a + b), quote(if (a) b)))
  show(as.call(list(as.name(name), x = 1, y = 2)))
  show(as.call(list(as.name(name), NULL)))
  show(as.call(list(as.name(name), pairlist(a = 1, b = quote(expr = )), quote(a))))
  show(as.call(list(as.name(name), NULL, quote(a), "src")))
  show(as.call(list(as.name(name), list(a = 1), quote(a))))
  show(as.call(list(as.name(name), quote(f(a = 1, b)), quote(a))))
}
print(tryCatch(print(call("function", 1)), error = function(e) conditionMessage(e)))
for (bt in c(TRUE, FALSE)) { cat(deparse(call("+", 1, 2, 3), backtick = bt), deparse(as.call(list(as.name("%in%"), x = 1, y = 2)), backtick = bt), deparse(call("[<-", quote(x), 1, 2), backtick = bt), deparse(call("names<-", quote(x), "a"), backtick = bt), deparse(call("my fun", 1), backtick = bt), sep = "\n") }
