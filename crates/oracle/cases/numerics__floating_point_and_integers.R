0.1 + 0.2 == 0.3
isTRUE(all.equal(0.1 + 0.2, 0.3))
1 / 0
-1 / 0
0 / 0
sqrt(2)^2 == 2
5 %/% 0
5L %/% 0L
5L %% 0L
bitwAnd(12L, 10L)
2147483647L
tryCatch(as.integer(2^31), warning = function(w) conditionMessage(w))
round(2.5); round(3.5); round(-2.5)
round(1234.567, -2)
signif(123456, 2)
format(2/3, digits = 3)
print(pi, digits = 10)
1e-5
100000
1e5
123456789012
0.1234567891234
c(1.5, NA, 3)
exp(1)
factorial(10)
choose(5, 2)
10 %in% c(1, 10)
