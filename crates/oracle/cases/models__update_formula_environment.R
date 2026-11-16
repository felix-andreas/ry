# update.formula() keeps the environment of the old formula, not the new one.
g <- function() {
  fo <- x ~ y + z
  u <- local(update.formula(fo, ~ . - y))
  print(format(u))
  print(identical(environment(u), environment()))
  tmp <- .Call(stats:::C_updateform, fo, local(~ . + w))
  print(identical(environment(tmp), environment()))
  print(names(attributes(tmp)))
}
g()
print(update(y ~ x, ~ . + z))
