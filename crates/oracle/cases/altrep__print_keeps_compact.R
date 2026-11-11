# Printing a compact sequence reads its elements without expanding it, as GNU R's print does,
# while format() expands it through its data pointer. The address is dropped from inspect().
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
