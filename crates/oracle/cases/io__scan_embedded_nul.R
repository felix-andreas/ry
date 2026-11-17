# A nul in the input ends the field it is in, and scan() warns once at the end; with skipNul
# the nuls are dropped instead. readTableHead names each line a nul cut short.
f <- tempfile()
writeBin(as.raw(c(0x61, 0x2c, 0x62, 0x0a, 0x78, 0x00, 0x79, 0x2c, 0x7a, 0x0a, 0x0a,
                  0x31, 0x00, 0x32, 0x2c, 0x00, 0x0a, 0x70, 0x2c, 0x71, 0x0a)), f)
print(scan(f, what = "", sep = ",", quiet = TRUE))
print(scan(f, what = "", sep = ",", quiet = TRUE, skipNul = TRUE))
print(scan(f, what = "", quiet = TRUE))
print(scan(f, what = list("", ""), sep = ",", quiet = TRUE))
print(read.csv(f))
print(read.csv(f, skipNul = TRUE))
print(count.fields(f, sep = ","))
writeBin(as.raw(c(0x31, 0x00, 0x32, 0x20, 0x33, 0x0a)), f)
print(scan(f, quiet = TRUE))
writeBin(as.raw(c(0x22, 0x61, 0x00, 0x62, 0x22, 0x0a)), f)
print(scan(f, "", quiet = TRUE))
unlink(f)
