# oracle: requires rlang
# `.Call` into a package's registered native routine: through its `NativeSymbolInfo`, its bare
# address, a name with `PACKAGE`, tagged arguments, the arity check, and repeated calls from a
# compiled loop.
length_routine <- rlang:::ffi_length
print(class(length_routine))
print(.Call(length_routine, 1:3))
print(.Call(length_routine$address, letters))
print(.Call("ffi_length", list(1, 2), PACKAGE = "rlang"))
print(.Call(length_routine, x = 1:5))
print(tryCatch(.Call(length_routine), error = conditionMessage))
print(tryCatch(.Call(length_routine, 1, 2), error = conditionMessage))
print(tryCatch(.Call(length_routine, 1, PACKAGE = "rlang"), error = conditionMessage))
print(tryCatch(.Call("no_such_routine", 1, PACKAGE = "rlang"), error = conditionMessage))
string_routine <- rlang:::ffi_is_string
print(.Call(string_routine, "a", NULL, NULL))
print(.Call(string_routine, 1, NULL, NULL))
total <- function(values) {
  count <- 0L
  for (value in values) count <- count + .Call(length_routine, value)
  count
}
for (round in 1:3) print(total(list(1:3, NULL, letters, list(1, 2))))
