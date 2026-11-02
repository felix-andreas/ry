# The placeholder may head a chain of extractions on a pipe's right side.
fit <- list(coef = c(a = 1, b = 2), info = list(n = 3))
print(fit |> _$coef)
print(fit |> _$coef[[2]])
print(fit |> _[["info"]]$n)
print(list(1:3) |> _[][[1]])
print(quote(x |> _$a[[b]]))
for (text in c("x |> _$a[[_]]", "x |> f(_)$a", "x |> _ + 1", "x |> (_)$a")) {
  cat(tryCatch(deparse(parse(text = text)[[1]]), error = conditionMessage), "\n", sep = "")
}
