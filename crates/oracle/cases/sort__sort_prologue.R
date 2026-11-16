# `sort()`, `sort.default()` and `sort.int()` on plain vectors of every type, with every
# `method`, `na.last`, `decreasing`, `partial` and `index.return`, the attributes the result
# keeps, and the shapes that dispatch or that the R bodies report.
show <- function(expr) {
  value <- tryCatch(expr, error = function(e) paste("Error in", deparse(conditionCall(e))[1L], ":", conditionMessage(e)))
  str(value)
  print(attributes(value))
}

lgl <- c(TRUE, NA, FALSE, TRUE, FALSE)
int <- c(3L, 1L, NA, 2L, 10L, 1L)
dbl <- c(2.5, NA, -Inf, NaN, 0, -0, Inf, 1, 2.5)
chr <- c("pear", NA, "apple", "Apple", "éclair", "zoo", "", "apple")
cpl <- c(2+1i, 1-1i, NA, 1+0i, 2+1i)

for (x in list(lgl, int, dbl, chr, cpl)) {
  show(sort(x))
  show(sort(x, decreasing = TRUE))
  show(sort(x, na.last = TRUE))
  show(sort(x, na.last = FALSE))
  show(sort(x, TRUE, na.last = TRUE))
  show(sort(x, method = "radix"))
  show(sort(x, method = "radix", na.last = FALSE, decreasing = TRUE))
  show(sort(x, method = "shell"))
  show(sort(x, index.return = TRUE))
  show(sort(x, index.return = TRUE, na.last = TRUE))
  show(sort(x, method = "radix", index.return = TRUE))
  show(sort.int(x))
  show(sort.int(x, na.last = TRUE, decreasing = TRUE))
  show(sort.default(x, na.last = FALSE))
}
show(1 / sort(c(0, -0, 0, -0)))
show(1 / sort(c(-0, 0), decreasing = TRUE))
show(sort(c(3, 1, 2), method = "quick"))
show(sort(c(3, 1, 2), partial = 2))
show(sort(c(3L, 1L, 2L), method = "radix", partial = 2))

# Names are kept, every other attribute dropped; NAs keep their names.
show(sort(c(b = 2L, a = 1L, c = NA, d = 0L)))
show(sort(c(b = 2L, a = 1L, c = NA, d = 0L), na.last = TRUE))
show(sort(c(b = 2, a = 1, c = NA), na.last = FALSE, decreasing = TRUE))
show(sort(c(b = "x", a = NA, c = "a"), na.last = FALSE))
show(sort(c(b = "x", a = NA, c = "a", d = "x"), na.last = TRUE, decreasing = TRUE))
show(sort(c(p = 2+0i, q = NA, r = 1+0i), na.last = TRUE))
show(sort(c(b = 2, a = 1), index.return = TRUE))
show(sort(structure(c(3, 1, 2), foo = "bar")))
show(sort(structure(c("c", "a"), foo = "bar", names = c("p", "q"))))
show(sort(structure(c(z = 3L, y = 1L), extra = 1)))
m <- matrix(c(4, 2, 3, 1), 2L, dimnames = list(c("a", "b"), NULL))
show(sort(m))
show(sort(matrix(c("d", "b", "c", "a"), 2L)))
show(sort.int(matrix(4:1, 2L), decreasing = TRUE))

# Empty, one-element and compact vectors.
show(sort(integer()))
show(sort(character()))
show(sort(numeric(), index.return = TRUE))
show(sort(5))
show(sort(NA))
show(sort(c(NA, NA)))
show(sort(NA_character_, na.last = TRUE))
show(sort(1:10))
show(sort(10:1))
show(sort(seq_len(6), decreasing = TRUE))
show(sort(NULL))
show(sort.int(NULL))
show(sort(NULL, method = "radix"))
x <- sort(c(3L, 1L, 2L))
show(is.unsorted(x))
show(identical(x, 1:3))
show(identical(sort(c(b = 2, a = 1)), c(a = 1, b = 2)))

# Objects dispatch: factors, ordered factors, dates, times, a class with its own methods.
f <- factor(c("b", "a", NA, "c", "a"), levels = c("c", "b", "a"))
show(sort(f))
show(sort(f, decreasing = TRUE, na.last = TRUE))
show(sort.int(f))
show(sort(factor(c("lo", "hi", "mid"), levels = c("lo", "mid", "hi"), ordered = TRUE)))
show(sort(as.Date(c("2024-03-01", "2023-01-05", NA, "2024-01-01"))))
show(sort(as.Date(c("2024-03-01", "2023-01-05")), decreasing = TRUE))
show(sort(as.POSIXct(c(5, 1, 3), origin = "1970-01-01", tz = "UTC")))
df <- data.frame(g = c(2, 1, 2, 1), h = c("b", "a", "a", "b"))
show(sort(df$g))
show(sort(df$h, decreasing = TRUE))
sort.myclass <- function(x, decreasing = FALSE, ...) "sort.myclass"
show(sort(structure(1:3, class = "myclass")))
xtfrm.backwards <- function(x) -unclass(x)
"[.backwards" <- function(x, i) structure(unclass(x)[i], class = "backwards")
show(sort(structure(c(1, 3, 2), class = "backwards")))
sort.numeric <- function(x, decreasing = FALSE, ...) "sort.numeric"
show(sort(c(3, 1, 2)))
show(sort(3:1))
show(sort(c("b", "a")))
rm(sort.numeric)
show(sort(c(3, 1, 2)))

# Errors and their calls.
show(sort(list(2, 1)))
show(sort(as.raw(c(3, 1))))
show(sort(c(3, 1), decreasing = NA))
show(sort(c(3, 1), decreasing = c(TRUE, FALSE)))
show(sort(c(3, 1), decreasing = "yes"))
show(sort.int(c(3, 1), decreasing = NA))
show(sort.int(c(3, 1), index.return = NA))
show(sort(c("b", NA), index.return = TRUE, na.last = TRUE))
show(sort(c(3, 1), method = "radix", partial = 1))
show(sort(c(1i, 2i), method = "radix"))
show(sort(c(3, 1), method = "bogus"))
show(sort(c(3, 1), na.last = "x"))
show(sort())
show(sort.int())

# Promises are forced as the R bodies force them.
trace <- character()
note <- function(label, value) { trace <<- c(trace, label); value }
show(sort(note("x", c(2, 1)), decreasing = note("decreasing", FALSE), na.last = note("na.last", TRUE)))
show(trace)
trace <- character()
show(sort.int(note("x", c("b", "a")), decreasing = note("decreasing", TRUE), na.last = note("na.last", TRUE), index.return = note("index.return", FALSE)))
show(trace)
trace <- character()
show(sort.int(note("x", c(TRUE, FALSE)), na.last = note("na.last", TRUE), method = "radix", index.return = note("index.return", TRUE)))
show(trace)
show(tryCatch(sort(c(2, 1), na.last = stop("boom")), error = conditionMessage))

# Calls in loops and inside functions, and results built on.
total <- 0
for (i in 1:200) total <- total + sort(c(i, 2, 1))[3L]
show(total)
k <- function(v, d) sort(v, decreasing = d)
show(k(c(5L, 3L, 9L), TRUE))
show(rev(sort(c(3L, 1L, 2L))))
show(sort(unique(c(3L, 1L, 3L, 2L))))
show(withVisible(sort(3:1))$visible)
y <- sort(c(3L, 1L, 2L))
y[1] <- 10L
show(y)
