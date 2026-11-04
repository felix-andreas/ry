# expect: type-mismatch
# The number types never convert implicitly.
count <- 3i64
price <- 2.5f64
print(count * price)
