# expect: ok
# A literal accumulator takes its type from the generic call it flows through.
total <- 0
xs <- [1.5, 2.5]
total <- List.walk(xs, total, \(sum, x) sum + x)
print(total)
#> [1] 4
