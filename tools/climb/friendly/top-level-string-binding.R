# expect: ok
# In a typed file a top-level string literal is a Str.
name <- "ry"
greet <- function(who: Str) -> Str Str.concat("hello ", who)
print(greet(name))
#> [1] "hello ry"
