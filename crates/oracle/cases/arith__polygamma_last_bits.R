# The polygamma functions run GNU R's port of Amos's algorithm, so they agree to the last bit,
# including the reflection for negative arguments and the asymptotic branch for large ones.
x <- c(-10.5, -3.7, -1.5, -1e-8, 1e-300, 1e-9, 0.01, 0.5, 1, 1.4616321, 2.5, 7.9, 17.3, 99.5,
       1e5, 1e16, 1e300, Inf, -Inf, NaN, NA, 0, -2)
show <- function(values) cat(sprintf("%a", values), fill = 80)
show(suppressWarnings(digamma(x)))
show(suppressWarnings(trigamma(x)))
for (deriv in c(2, 3, 5, 6, 30)) show(suppressWarnings(psigamma(x, deriv)))
print(psigamma(1, 101))
print(digamma(matrix(c(0.5, 2, 3, 4), 2, dimnames = list(NULL, c("a", "b")))))
