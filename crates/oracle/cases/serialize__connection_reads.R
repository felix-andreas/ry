# Objects read one after another from one connection: each read stops exactly after its object,
# so the next read, a raw read, or a restore hook that reads the same connection sees the right
# bytes. Covers gzip files, plain files, raw connections, and every stream format.
path <- tempfile(fileext = ".rds")
con <- gzfile(path, "wb")
saveRDS(list(a = 1:3, b = "x"), con)
saveRDS(seq(0.5, 5e4, by = 0.5), con)
writeBin(c(7L, 8L), con)
saveRDS(letters, con)
close(con)
con <- gzfile(path, "rb")
str(readRDS(con))
big <- readRDS(con); print(length(big)); print(sum(big))
print(readBin(con, "integer", 2))
print(readRDS(con)[24:26])
print(length(readBin(con, "raw", 10)))
close(con)

plain <- tempfile()
con <- file(plain, "wb")
for (format in c(FALSE, TRUE)) serialize(list(format, 1:5), con, ascii = format)
serialize(mtcars[1:3, 1:3], con, xdr = FALSE)
close(con)
con <- file(plain, "rb")
for (i in 1:3) str(unserialize(con))
close(con)

con <- rawConnection(raw(0), "wb")
serialize(1:10, con); serialize(c(x = 2.5), con)
bytes <- rawConnectionValue(con)
close(con)
con <- rawConnection(bytes, "rb")
print(unserialize(con)); print(unserialize(con)); print(length(readBin(con, "raw", 4)))
close(con)

# A restore hook runs in the middle of a read, and the read carries on after it.
env <- new.env()
hook_out <- function(object) if (identical(object, env)) "env" else NULL
path <- tempfile()
con <- gzfile(path, "wb")
serialize(list(env, 42), con, refhook = hook_out)
writeBin(99L, con)
close(con)
con <- gzfile(path, "rb")
restored <- unserialize(con, refhook = function(name) { print(name); environment() })
print(class(restored[[1]])); print(restored[[2]])
print(readBin(con, "integer", 1))
close(con)

