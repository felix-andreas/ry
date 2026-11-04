# expect: ok
# A literal arm adapts to the type a binding annotation expects, as it does in a function's tail.
first: I64 <- match (List.first([9i64])) { :Ok(x) => x, :Err(_) => 6 }
print(first)
#> 9i64
