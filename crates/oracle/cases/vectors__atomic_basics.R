x <- c(a = 1, b = 2, c = 3)
x
x[c("a", "c")]
x[-1]
x[x > 1]
typeof(1:3)
typeof(c(1L, 2.5))
typeof(c(1, "a"))
typeof(c(TRUE, 1L))
1:0
seq(10, 1, by = -3)
rev(seq_len(4))
integer(0)
sum(integer(0))
max(-Inf, numeric(0))
as.integer(3.9)
as.integer(-3.9)
5 %/% 2
-5 %/% 2
-5 %% 3
5L / 2L
class(5L / 2L)
.Machine$integer.max + 1L
