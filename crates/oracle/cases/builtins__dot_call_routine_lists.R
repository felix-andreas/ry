# `.Call` takes its routine from a `NativeSymbolInfo` list: one a namespace registered, a copy
# of it, and the address alone.
routine <- utils:::C_crc64
print(names(routine))
for (i in 1:3) print(.Call(routine, as.character(i)))
copy <- structure(unclass(routine)[seq_along(routine)], class = class(routine))
for (i in 1:3) print(.Call(copy, as.character(i)))
call_it <- function(routine, text) .Call(routine, text)
for (i in 1:3) print(call_it(routine, "hello"))
for (i in 1:3) print(call_it(copy, "hello"))
for (i in 1:3) print(call_it(routine$address, "hello"))

cat("-- a list that is no routine\n")
unclassed <- unclass(routine)
print(tryCatch(.Call(unclassed, "hello"), error = function(e) "an error"))
plain <- structure(list(name = "crc64", address = "nowhere"), class = "NativeSymbolInfo")
print(tryCatch(.Call(plain, "hello"), error = function(e) "an error"))
empty <- structure(list(), class = "NativeSymbolInfo")
print(tryCatch(.Call(empty, "hello"), error = function(e) "an error"))

cat("-- named arguments are passed by position\n")
for (i in 1:3) print(.Call(routine, x = "hello"))
for (i in 1:3) print(.Call(routine, anything = as.character(i)))
for (i in 1:3) print(.Call(routine, "hello", PACKAGE = "utils"))
