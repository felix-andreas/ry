# expect: type-mismatch
# A List<I64> is no R vector, so passing it to an R function is a boundary error.
counts <- [1, 2, 3]
print(sum(counts))
