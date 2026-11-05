# match.arg with the default choices, an exact match, a unique prefix, and every error path.
f <- function(type = c("linear", "quadratic", "cubic")) match.arg(type)
print(f()); print(f("quad")); print(f("cubic")); print(f(c("linear", "quadratic", "cubic")))
for (bad in list("q2", "", NA_character_, c("linear", "cubic"), 1, NULL)) {
  print(tryCatch(f(bad), error = conditionMessage))
}
g <- function(kind = c("aa", "ab", "b")) match.arg(kind)
print(tryCatch(g("a"), error = conditionMessage)); print(g("ab")); print(g("b"))
print(match.arg("me", c("mean", "median"), several.ok = FALSE) |> tryCatch(error = conditionMessage))
print(match.arg(c("mean", "med"), c("mean", "median"), several.ok = TRUE))
print(match.arg("med", c("mean", "median")))
print(match.arg(NULL, c("x", "y")))
named <- c(first = "x", second = "y"); print(match.arg("y", named))
h <- function(how = c("été", "hiver")) match.arg(how)
print(h()); print(h("hi"))
# A computed default, a default naming a local, and a non-constant arg expression.
k <- function(mode = toupper(c("a", "b"))) match.arg(mode)
print(k()); print(k("B"))
m <- function(x = choices) { choices <- c("left", "right"); match.arg(x) }
print(m()); print(m("ri"))
n <- function(side = c("left", "right")) match.arg(paste0("ri", "ght"), c("left", "right"))
print(n())
# The order arg and choices are forced in.
p <- function(a, b) match.arg({ cat("arg\n"); a }, { cat("choices\n"); b })
print(p("x", c("x", "y")))
q <- function(v = c("one", "two")) { r <- match.arg(v); invisible(r) }
print(withVisible(q("two"))$visible); print(withVisible(match.arg("x", c("x", "y")))$visible)
print(tryCatch(match.arg("a", c("ab", "ac")), error = function(e) deparse(conditionCall(e))))
