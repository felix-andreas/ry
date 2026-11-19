# model.matrix writes each design column once: numeric, logical and factor parts, interactions
# whose first part varies fastest, matrix variables, full codings without an intercept, and
# row names that are read lazily.
d <- data.frame(x = c(1.5, -2, 0, 4, NA, 3), z = 1:6, b = c(TRUE, FALSE, TRUE, NA, FALSE, TRUE),
                f = factor(c("u", "v", "w", "u", "v", "w")), g = factor(c("p", "q", "q", "p", "q", "p")))
d$m <- cbind(a = 6:1, c = c(0.5, 1, 2, 4, 8, 16))
show <- function(formula, data = d, ...) {
  mm <- model.matrix(formula, model.frame(formula, data, na.action = na.pass), ...)
  print(mm)
}
show(~ x + f)
show(~ f:g)
show(~ x:f + z)
show(~ 0 + f + g)
show(~ f * g * b)
show(~ m + x)
show(~ m:f)
show(~ poly(z, 2))
show(~ f, contrasts.arg = list(f = "contr.sum"))
show(~ 1)
show(~ 0 + b)
e <- d; rownames(e) <- letters[1:6]
show(~ x + g, e)
mm <- model.matrix(~ x, data.frame(x = seq_len(20000) / 7))
print(dim(mm)); print(head(rownames(mm))); print(tail(rownames(mm))); print(sum(mm))
