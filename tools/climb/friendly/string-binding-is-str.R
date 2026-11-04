# expect: ok
# A string literal bound to a name becomes the Str a later annotation asks for.
greeting <- "hi"
message: Str <- greeting
print(message)
#> [1] "hi"
