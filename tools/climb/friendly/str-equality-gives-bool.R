# expect: ok
# Comparing a Str list with a string literal gives a List<Bool>.
names <- ["a", "b"]
print(names == "a")
#> [TRUE, FALSE]
