# oracle: requires fastmap
# `.Call` of a compiled package routine through its `NativeSymbolInfo` list: the one the
# namespace registered, a rebuilt copy of it, and the address alone.
# Named arguments go to the routine by position.
to_utf8 <- fastmap:::C_char_vec_to_utf8
print(names(to_utf8))
print(to_utf8$numParameters)
for (i in 1:3) print(.Call(to_utf8, c("key", letters[i])))
convert <- function(routine, x) .Call(routine, x)
for (i in 1:3) print(convert(to_utf8, letters[i]))

cat("-- a rebuilt copy\n")
copy <- structure(unclass(to_utf8)[seq_along(to_utf8)], class = class(to_utf8))
for (i in 1:3) print(.Call(copy, letters[i + 1L]))
for (i in 1:3) print(convert(copy, letters[i + 2L]))

cat("-- the address alone\n")
for (i in 1:3) print(convert(to_utf8$address, letters[i]))

cat("-- three arguments, named and not\n")
map <- .Call(fastmap:::C_map_create)
map_set <- fastmap:::C_map_set
map_get <- fastmap:::C_map_get
for (i in 1:3) .Call(map_set, map, letters[i], i)
for (i in 1:4) print(.Call(map_get, map, letters[i]))
for (i in 1:3) .Call(map_set, map = map, key = LETTERS[i], idx = 10L + i)
for (i in 1:4) print(.Call(map_get, anything = map, LETTERS[i]))
lookup <- function(key) .Call(map_get, map = map, key = key)
for (i in 1:3) print(lookup(letters[i]))
for (i in 1:3) print(.Call(map_get, map, LETTERS[i], PACKAGE = "fastmap"))
with_package <- function(key) .Call(map_get, map, key, PACKAGE = "fastmap")
for (i in 1:3) print(with_package(letters[i]))

cat("-- the wrong number of arguments is an error\n")
print(tryCatch(.Call(to_utf8, "a", "b"), error = conditionMessage))
print(tryCatch(.Call(copy), error = conditionMessage))
print(tryCatch(.Call(map_get, map = map, key = "a", more = 1L), error = conditionMessage))
print(tryCatch(lookup(), error = function(e) "an error"))

cat("-- a finalizer that became ready runs once\n")
count <- 0L
invisible(local({
    scratch <- new.env()
    reg.finalizer(scratch, function(e) count <<- count + 1L)
}))
invisible(gc())
for (i in 1:3) invisible(.Call(to_utf8, letters[i]))
invisible(gc())
print(count)
