hex <- paste0(
  "504b030414000000080083182258fb1650dd12000000100000000e000000646174612f7461626c652e6373764bd449e2",
  "32d4a9e032d2a9e432d6a9e20200504b030414000000000083182258c588ca3f08000000080000000a00000073746f72",
  "65642e62696e0102030405060708504b0102140314000000080083182258fb1650dd12000000100000000e0000000000",
  "000000000000800100000000646174612f7461626c652e637376504b0102140314000000000083182258c588ca3f0800",
  "0000080000000a000000000000000000000080013e00000073746f7265642e62696e504b050600000000020002007400",
  "00006e0000000000"
)
bytes <- as.raw(strtoi(substring(hex, seq(1, nchar(hex), 2), seq(2, nchar(hex), 2)), 16L))
archive <- tempfile(fileext = ".zip")
writeBin(bytes, archive)
print(read.csv(unz(archive, "data/table.csv")))
con <- unz(archive, "stored.bin", "rb")
print(readBin(con, "raw", 100))
print(summary(con)[c("class", "mode", "text", "opened", "can write")])
close(con)
con <- unz(archive, "data/table.csv")
open(con)
print(readLines(con, 2))
print(readLines(con))
close(con)
message <- tryCatch(readLines(unz(archive, "absent.txt")), warning = conditionMessage)
print(sub(archive, "<archive>", message, fixed = TRUE))
