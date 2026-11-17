# A string whose bytes are not valid UTF-8, such as Latin-1 text read in a UTF-8 session, keeps
# its bytes through output, symbols, connections and the table readers.
x <- rawToChar(as.raw(c(0x61, 0xff, 0x62)))
path <- tempfile()
cat(x, file = path)
readBin(path, "raw", 10)
cat(x, x, "\n", file = path, sep = "|")
readBin(path, "raw", 20)
write(x, path)
readBin(path, "raw", 10)
writeLines(x, path)
readBin(path, "raw", 10)
charToRaw(as.character(as.name(x)))
charToRaw(names(setNames(1, x)))
charToRaw(paste0(x, x))
charToRaw(conditionMessage(tryCatch(stop(x), error = identity)))
con <- rawConnection(raw(), "w")
cat(x, file = con)
writeLines(x, con)
rawConnectionValue(con)
close(con)

# strsplit refuses the string unless it splits bytes.
strsplit(c(x, "ok"), "")
strsplit(c(x, "ok"), "b")
strsplit(c(x, "ok"), "b", fixed = TRUE)
lapply(strsplit(x, "b", fixed = TRUE, useBytes = TRUE)[[1]], charToRaw)

# The readers keep a Latin-1 file's bytes, and `encoding` marks them.
writeBin(as.raw(c(0x61, 0xe4, 0x3b, 0x62, 0x0a, 0x63, 0x3b, 0xfc, 0x0a)), path)
lapply(readLines(path), charToRaw)
lapply(scan(path, "", sep = ";", quiet = TRUE), charToRaw)
frame <- read.csv2(path, header = FALSE)
lapply(frame$V1, charToRaw)
iconv(frame$V1, "latin1", "UTF-8")
marked <- scan(path, "", sep = ";", quiet = TRUE, encoding = "latin1")
marked
frame <- read.table(path, sep = ";", encoding = "latin1")
frame$V1
nchar(frame$V1)
read.table(path, sep = ";", fileEncoding = "latin1")
converted <- type.convert(marked, as.is = TRUE)
converted
levels(type.convert(marked, as.is = FALSE))

# pushBack returns the bytes it was given; a Latin-1 string is re-encoded unless pushed as bytes.
con <- file(path, "rt")
line <- readLines(con, 1)
pushBack(c(line, line), con, encoding = "bytes")
lapply(readLines(con, 2), charToRaw)
pushBack(marked[1], con)
charToRaw(readLines(con, 1))
pushBack(marked[1], con, encoding = "bytes")
charToRaw(readLines(con, 1))
close(con)
unlink(path)
