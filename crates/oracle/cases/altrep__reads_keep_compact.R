# Printing and subsetting a compact sequence read its elements without expanding it, as GNU R
# does, while format() expands it through its data pointer. The address is dropped from inspect().
state <- function(v) sub("^@[0-9a-f]+ ", "", capture.output(.Internal(inspect(v)))[1])
x <- 1:10
print(x)
state(x)
y <- seq_len(300)
print(y)
state(y)
z <- seq(2, 20, by = 2)
print(z)
z <- 5:1
print(z)
state(z)
f <- seq_len(3)
format(f)
state(f)
# Subsetting reads elements too.
s <- 1:10
s[3]
s[[4]]
s[c(2, 12)]
state(s)
# So does a for loop, at top level as inside a function.
total <- 0L
for (i in s) total <- total + i
total
state(s)
d <- as.double(1:5)
for (v in d) total <- total + v
total
state(d)
# Summaries and mean() read a sequence by region and leave it compact, while range() combines
# its arguments with c() first, which expands it.
m <- 3:12
c(min(m), max(m), sum(m), prod(1:5), mean(m))
max(m, 20L)
min(m, -1)
state(m)
r <- as.double(10:1)
c(min(r), max(r), mean(r), sum(r, 0.5))
state(r)
g <- 4:9
range(g)
state(g)
