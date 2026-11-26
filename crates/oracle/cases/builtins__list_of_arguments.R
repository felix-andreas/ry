# `list()` of evaluated arguments: names from the call's tags, values stored without a copy
# that the caller could see change, and a names vector of the call's own.

str(list())
str(list(1L, "a", NULL, TRUE))
str(list(a = 1L))
str(list(a = 1L, 2, b = "x"))
str(list(`a b` = 1, "quoted" = 2, 3))
str(list(NULL, a = NULL))
str(list(f = sum, e = globalenv(), s = quote(x), l = quote(f(x))))
str(list(list(a = 1), b = list(2, c = 3)))

make <- function(first, second) list(first = first, second, third = first + 1L)
str(make(1L, "b"))
str(make(2L, NULL))

# A list keeps the value it was given when the variable changes, and the other way round.
numbers <- c(1, 2, 3)
kept <- list(values = numbers, numbers)
numbers[1] <- 9
print(kept)
kept$values[2] <- 7
print(numbers)
print(kept[[2]])

# Each call has a names vector of its own.
pair <- function(left, right) list(left = left, right = right)
one <- pair(1, 2)
two <- pair(3, 4)
names(one)[1] <- "changed"
print(names(one))
print(names(two))
print(names(pair(5, 6)))

# The same site with values of many kinds, enough times for the optimizing tier.
total <- 0L
for (index in 1:3000) {
    item <- list(key = index, label = "x", index)
    total <- total + length(item) + (names(item)[1] == "key") + item[[3]] %% 2L
}
print(total)
wrap <- function(value) list(value = value)
kinds <- list(1L, 2.5, "c", TRUE, NULL, 1i, as.raw(1), sum, c(a = 1))
for (round in 1:300) for (kind in kinds) wrap(kind)
str(lapply(kinds, wrap)[c(1, 5, 9)])

# Other ways to reach `list`.
through_dots <- function(...) list(...)
str(through_dots(a = 1, 2))
str(through_dots())
str(do.call("list", list(a = 1, 2)))
str(do.call(list, list(1, b = 2)))
str(base::list(a = 1))
str((list)(a = 1, 2))
str(Map(function(left, right) list(left = left, right), 1:2, c("a", "b")))

# An attribute on a value stays on the element.
tagged <- structure(1:2, class = "tag", note = "n")
item <- list(t = tagged, tagged)
print(attributes(item$t))
print(inherits(item[[2]], "tag"))

# An empty argument is an error.
print(tryCatch(list(a = 1, ), error = function(e) "error"))
print(tryCatch(list(, 1), error = function(e) "error"))

# A long call.
long <- list(a = 1, b = 2, c = 3, d = 4, e = 5, f = 6, g = 7, h = 8, i = 9, j = 10, 11, 12)
print(names(long))
print(sum(unlist(long)))

# A repeated name, numbered `...` elements, an alias and a hot untagged call.
str(list(a = 1, a = 2))
numbered <- function(...) list(a = ..1, ..2)
str(numbered(1, "b"))
alias <- list
str(alias(a = 1, 2))
pair <- function(x) list(x, x + 1)
for (i in 1:2000) pair(i)
str(pair(1))

# A builtin whose direct entry takes no names keeps its named call.
print(paste(a = "x", "y"))
