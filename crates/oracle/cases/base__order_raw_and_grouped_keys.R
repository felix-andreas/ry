# Raw vectors cannot be ordered, but a raw key after the first is read only on a tie.
r <- as.raw(c(3, 1, 2))
for (e in expression(order(r), order(r, method = "radix"), sort(r),
                     sort(r, method = "radix"), order(1:3, r),
                     order(1:3, r, method = "radix"), sort.list(r),
                     order(c(1, 1, 2), r, method = "radix")))
  print(tryCatch(eval(e), error = function(err) conditionMessage(err)))

# A dense leading key splits the rows into groups that sort on their own, so the result must
# stay stable within ties and honour each key's direction and NA placement.
set.seed(7)
n <- 2000
g <- sample(c(1:12, NA), n, TRUE)
x <- round(rnorm(n), 1)
x[sample(n, 40)] <- NA
s <- sample(c("b", "a", "c", NA), n, TRUE)
check <- function(o) cat(length(o), sum(o * seq_along(o)) %% 1000003, head(o, 8), "\n")
check(order(g, -x))
check(order(g, x, method = "radix"))
check(order(g, x, decreasing = c(TRUE, FALSE), method = "radix"))
check(order(g, x, na.last = FALSE, method = "radix"))
check(order(g, s, x, method = "radix"))
check(order(s, g, method = "radix"))
check(order(as.integer(x * 10), g, method = "radix"))
check(order(g, x, na.last = NA, method = "radix"))
small <- c(2L, 1L, 2L, 1L, 2L)
print(order(small, c(5, 4, 3, 2, 1), method = "radix"))
