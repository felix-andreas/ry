# axis() takes a call as one label, as GNU R coerces it to an expression.
pdf(NULL)
plot(1:10)
print(axis(4, at = 5, labels = quote(sqrt(epsilon[c])), las = 1))
print(try(axis(4, at = c(2, 5), labels = quote(sqrt(x)), las = 1), silent = TRUE))
invisible(dev.off())
