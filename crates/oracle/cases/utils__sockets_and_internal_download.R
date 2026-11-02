src <- tempfile()
writeLines(c("a", "b"), src)
dest <- tempfile()
print(download.file(paste0("file://", src), dest, method = "internal", quiet = TRUE))
print(download.file(paste0("file://", src), dest, method = "internal", mode = "a", quiet = TRUE))
print(readLines(dest))
hide <- function(message) gsub(src, "SRC", message, fixed = TRUE)
for (u in c("http://x.org/a", "https://x.org/a", "ftp://x/a", "foo://x", paste0("file://", src, "zz"))) {
  print(hide(tryCatch(download.file(u, dest, method = "internal", quiet = TRUE), error = conditionMessage)))
}
print(hide(tryCatch(download.file(paste0("file://", src), file.path(src, "x", "y"), method = "internal", quiet = TRUE), error = conditionMessage)))
for (u in c("http://x.org/a", "https://x.org/a", "ftps://x/a", "foo://x")) {
  print(tryCatch(url(u, method = "internal"), error = conditionMessage))
}
con <- url(paste0("file://", src), method = "internal")
print(readLines(con))
close(con)
port <- 40000L + Sys.getpid() %% 20000L
client <- tempfile(fileext = ".R")
writeLines(c(
  "Sys.sleep(1)",
  sprintf("s <- make.socket('localhost', %dL)", port),
  "write.socket(s, 'ping')",
  "reply <- read.socket(s)",
  "write.socket(s, paste0('got:', reply))",
  "Sys.sleep(0.5)",
  "close.socket(s)"
), client)
system2(file.path(R.home("bin"), "Rscript"), client, wait = FALSE, stdout = FALSE)
system2("true", wait = FALSE)
print(system("echo hi", intern = TRUE))
s <- make.socket(port = port, server = TRUE)
print(class(s))
print(attr(s$socket, "host"))
print(read.socket(s))
print(write.socket(s, "pong"))
print(read.socket(s, maxlen = 4L))
print(read.socket(s, loop = TRUE))
print(read.socket(s))
print(close.socket(s))
print(tryCatch(make.socket("localhost", 1L), error = conditionMessage))
unlink(c(src, dest, client))
