# `order()` on plain vectors of every type, with every `method`, `na.last` and `decreasing`,
# several keys and ties, and the shapes its R body reports or dispatches on: objects, lists,
# raw keys, differing lengths and bad flags.
show <- function(expr) {
  value <- tryCatch(expr, error = function(e) paste("Error in", deparse(conditionCall(e))[1L], ":", conditionMessage(e)))
  print(value)
}

lgl <- c(TRUE, NA, FALSE, TRUE, FALSE)
int <- c(3L, 1L, NA, 2L, 10L, 1L)
dbl <- c(2.5, NA, -Inf, NaN, 0, -0, Inf, 1, 2.5)
chr <- c("pear", NA, "apple", "Apple", "éclair", "zoo", "", "apple")
cpl <- c(2+1i, 1-1i, NA, 1+0i, 2+1i)

for (key in list(lgl, int, dbl, chr, cpl)) {
  show(order(key))
  show(order(key, decreasing = TRUE))
  show(order(key, na.last = FALSE))
  show(order(key, na.last = NA))
  show(order(key, decreasing = TRUE, na.last = FALSE))
  for (method in c("auto", "shell", "radix")) {
    show(order(key, method = method))
    show(order(key, method = method, decreasing = TRUE, na.last = NA))
  }
}

# Empty and one-element keys, compact sequences, names.
show(order(integer()))
show(order(character()))
show(order(5L))
show(order(1:10))
show(order(10:1))
show(order(seq_len(7), decreasing = TRUE))
show(order(c(b = 2, a = 1, c = 3)))
show(order())
show(order(NULL))
show(order(NULL, method = "radix"))

# Several keys: ties broken by later keys, stability, a vector `decreasing`.
a <- c(2L, 1L, 2L, 1L, 2L, 1L)
b <- c(1, 3, 3, 2, 1, 2)
s <- c("x", "y", "x", "y", "z", "x")
show(order(a, b))
show(order(a, b, decreasing = TRUE))
show(order(a, b, decreasing = c(TRUE, FALSE)))
show(order(a, b, decreasing = c(TRUE, FALSE), method = "radix"))
show(order(a, b, decreasing = c(TRUE, FALSE), method = "shell"))
show(order(a, s))
show(order(s, a, method = "radix"))
show(order(s, a, decreasing = c(FALSE, TRUE)))
show(order(a, a, a))
show(order(c(1, 1, 1, 1)))
show(order(c(1, 1, 1, 1), decreasing = TRUE))

# Flags the body coerces, recycles or rejects.
show(order(int, decreasing = 1))
show(order(int, decreasing = "TRUE"))
show(order(int, decreasing = NA))
show(order(int, decreasing = logical(0)))
show(order(int, decreasing = c(TRUE, FALSE)))
show(order(chr, decreasing = c(TRUE, FALSE)))
show(order(int, na.last = c(TRUE, FALSE)))
show(order(chr, na.last = NA))
show(order(chr, na.last = "a"))
show(order(int, method = "rad"))
show(order(int, method = "bogus"))
show(order(int, method = c("auto", "shell", "radix")))
show(order(1:3, 1:2))
show(order(c("a", "b"), 1:3))
show(order(c("a", "b"), 1:3, method = "radix"))
show(order(list(1, 2)))
show(order(as.raw(1:3)))
show(order(as.raw(1:3), method = "radix"))
show(order(c(1i, 2i), method = "radix"))
show(order(1:2, c(1i, 2i), method = "radix"))

# Objects go through `xtfrm`: factors, dates, times, data frame columns, a class of its own.
f <- factor(c("b", "a", "c", "a"), levels = c("c", "b", "a"))
show(order(f))
show(order(f, decreasing = TRUE))
show(order(as.Date(c("2024-03-01", "2023-01-05", NA, "2024-01-01"))))
show(order(as.POSIXct(c(5, 1, 3), origin = "1970-01-01", tz = "UTC")))
df <- data.frame(g = c(2, 1, 2, 1), h = c("b", "a", "a", "b"))
show(order(df$g, df$h))
show(df[order(df$g, df$h), ])
xtfrm.backwards <- function(x) -unclass(x)
bw <- structure(c(1, 3, 2), class = "backwards")
show(order(bw))
show(order(bw, method = "radix"))
m <- matrix(c(3L, 1L, 2L, 4L), 2L)
show(order(m))

# Promises are forced as the R body forces them.
trace <- character()
note <- function(label, value) { trace <<- c(trace, label); value }
show(order(note("x", c(2, 1)), note("y", c(1, 2)), decreasing = note("decreasing", FALSE), na.last = note("na.last", TRUE)))
show(trace)
trace <- character()
show(order(note("x", c(2, 1)), decreasing = note("decreasing", FALSE), na.last = note("na.last", TRUE)))
show(trace)
trace <- character()
show(order(note("x", c("b", "a")), decreasing = note("decreasing", FALSE), na.last = note("na.last", NA)))
show(trace)
show(tryCatch(order(c(2, 1), na.last = stop("boom")), error = conditionMessage))
f2 <- function(...) order(...)
show(f2(c(3, 1, 2), decreasing = TRUE))
g <- function(x, d) order(x, decreasing = d)
show(tryCatch(g(c(3, 1, 2)), error = conditionMessage))

# Calls in loops and inside functions.
total <- 0L
for (i in 1:200) total <- total + order(c(i, 2L, 1L))[1L]
show(total)
h <- function(v) { out <- integer(); for (k in 1:3) out <- c(out, order(v, decreasing = k == 2L)); out }
show(h(c(5, 3, 9)))
show(identical(order(c(3L, 1L, 2L)), c(2L, 3L, 1L)))
show(withVisible(order(3:1))$visible)

# `order` has no quick sort, and a key that is no vector is reported before the lengths are.
show(order(c(3, 1, 2), method = "quick"))
show(order(c(3, 1, 2), 1:3, method = "quick"))
show(order(c("b", "a"), method = "quick"))
show(order(sum))
show(order(quote(a)))
show(order(new.env()))
show(order(1:2, new.env()))
show(order(1:2, NULL))
show(order(1:2, method = "shell", new.env()))
show(order(1:2, method = "shell", function(x) x))
show(order(list(1, 2)))
show(order(1:2, list(1, 2, 3)))
show(order(expression(a, b), method = "shell"))
