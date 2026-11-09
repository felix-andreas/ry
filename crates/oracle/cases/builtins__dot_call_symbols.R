# `.Call` resolves `.NAME` from a string, a `NativeSymbolInfo` list or an
# external pointer; the routine's name is read only for the arity error.
print(.Call(utils:::C_crc64, "hello"))
sym <- utils:::C_crc64
print(class(sym))
print(.Call(sym$address, "hello"))
r <- tryCatch(.Call(sym, "a", "b"), error = conditionMessage)
print(r)
r <- tryCatch(.Call(utils:::C_crc64), error = conditionMessage)
print(r)
r <- tryCatch(.Call(42), error = conditionMessage)
print(r)
r <- tryCatch(.Call(), error = conditionMessage)
print(r)
print(round(.Call(stats:::C_DoubleCentre, matrix(c(0, 1, 4, 1, 0, 1, 4, 1, 0), 3)), 10))
r <- tryCatch(.Call(stats:::C_DoubleCentre), error = conditionMessage)
print(r)
for (i in 1:3) print(.Call(utils:::C_crc64, as.character(i)))
