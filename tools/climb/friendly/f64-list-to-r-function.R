# expect: type-mismatch
# A ry list passed to an R function is a boundary error: convert it with List.to_r().
prices <- [1.5, 2.5]
print(mean(prices))
