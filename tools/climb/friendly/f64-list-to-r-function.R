# expect: ok
# A List<F64> is an R double vector at run time, so it passes to R code unconverted.
prices <- [1.5, 2.5]
print(mean(prices))
#> [1] 2
