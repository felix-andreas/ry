# expect: ok
# A string literal bound to a name becomes the Str a later annotation asks for.
main <- function() -> Str {
  greeting <- "hi"
  message: Str <- greeting
  message
}
print(main())
#> [1] "hi"
