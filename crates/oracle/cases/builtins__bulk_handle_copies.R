# `c`, `unlist` and `rep` over character vectors, lists, expressions, complex and raw vectors
# copy whole runs of elements; the results, names and sharing must match one-at-a-time copies.
strings <- c(a = "x", b = NA, c = "", d = "long string here")
plain <- c("p", "q", NA, "r")
values <- list(1L, "two", NULL, c(3, 4), list(5), quote(sym), function(x) x)
calls <- expression(a + b, f(x), 1)

show <- function(value) { str(value); cat(deparse(attributes(value)), "\n") }

show(c(plain, plain))
show(c(strings, plain, character(0), strings))
show(c(plain, "z", NULL, NA_character_))
show(c(plain, 1L, TRUE))
show(c(values, values))
show(c(values, list(a = 1), plain))
show(c(calls, calls))
show(c(calls, quote(g(y))))
show(unlist(list(plain, plain)))
show(unlist(list(strings, x = plain, list(y = "deep", plain))))
show(unlist(list(plain, 1:2), use.names = FALSE))
show(unlist(list(values, values), recursive = FALSE))

show(rep(plain, 3))
show(rep(strings, 2))
show(rep(plain, length.out = 10))
show(rep(plain, length.out = 2))
show(rep(plain, each = 2))
show(rep(plain, times = c(0, 2, 1, 3)))
show(rep(character(0), 4))
show(rep(values, 2))
show(rep(values, length.out = 3))
show(rep(calls, 2))
show(rep(c(1+2i, NA, -3i), 3))
show(rep(as.raw(c(1, 255, 0)), length.out = 8))
show(rep.int(plain, 2))
show(rep.int(values, 2L))
show(rep_len(plain, 9))
show(rep_len(as.raw(1:3), 7))
show(rep_len(c(1i, 2), 5))
show(rep(factor(c("u", "v", NA)), 2))
show(rep(as.character(1:5), 2))
show(c(as.character(1:5), as.character(c(1.5, NA))))

# A repeated list shares its elements with the original; changing a copy leaves both alone.
original <- list(c(1, 2, 3), "s")
repeated <- rep(original, 3)
repeated[[1]][2] <- 99
combined <- c(original, original)
combined[[3]][1] <- -1
print(original)
print(repeated[c(1, 3)])
print(combined[c(1, 3)])

# Large enough for the collector to run between the copies.
big <- sprintf("id%05d", 1:50000)
doubled <- c(big, big)
tripled <- rep(big, 3)
flat <- unlist(list(big, big, "end"))
for (round in 1:20) junk <- c(doubled, tripled)
print(c(length(doubled), length(tripled), length(flat), length(junk)))
print(identical(doubled, big[c(1:50000, 1:50000)]))
print(identical(tripled, big[rep(1:50000, 3)]))
print(c(doubled[c(1, 50000, 50001, 1e5)], tripled[c(100001, 150000)], flat[100001]))
cells <- lapply(1:20000, function(i) c(i, i))
twice <- rep(cells, 2)
both <- c(cells, cells)
for (round in 1:20) junk <- rep(cells, 4)
print(c(length(twice), length(both), sum(unlist(twice)), sum(unlist(both)), length(junk)))

# Gathers of character vectors, lists and expressions by position, past the end and by NA.
show(plain[c(4, 1, 1, 9, NA, 2)])
show(strings[c(2, 2, 7, 1)])
show(values[c(7, 1, 12, 3)])
show(calls[c(3, 1, 5)])
show(plain[-2])
show(values[c(TRUE, FALSE)])
show(plain[10:12])
show(rep(strings, each = 2))
show(rep(values, each = 2, length.out = 5))
show(rep(calls, times = c(2, 0, 1)))
show(rep(strings, times = 2, each = 2))
show(rev(values)[1:3])
show(head(big, 3)); show(tail(big, 2))
picked <- big[c(50000:49990, 60000, NA)]
for (round in 1:20) junk <- big[sample(50000)]
print(picked)
each <- rep(cells, each = 2)
each[[1]][1] <- 0
print(c(length(each), sum(unlist(each)), cells[[1]], each[[2]]))
