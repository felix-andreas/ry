# strrep, strtoi, utf8ToInt, charToRaw and `substr<-` on NA, recycled, named, empty and
# long inputs. A long as.character() result is a deferred vector, which each must expand.
x <- c(a = "ab", b = NA, c = "", d = "xyz")
print(strrep(x, 2))
print(strrep(x, c(0, 1, NA, 3)))
print(strrep("ab", 1:3))
print(strrep(c(a = "x", b = "y"), 1:4))
print(strrep(character(0), 3))
print(strrep("-", integer(0)))
print(tryCatch(strrep("a", -1), error = conditionMessage))
print(tryCatch(strrep(1, 2), error = conditionMessage))
print(strrep("é", 3))

print(strtoi(c("10", "077", "0x1F", "zz", NA, "", " 12", "2147483647", "2147483648", "-5")))
print(strtoi(c("777", "8"), 8L))
print(strtoi(c("ff", "FF", "g"), 16L))
print(strtoi("zz", 36L))
print(strtoi("101", base = 2))
print(tryCatch(strtoi("1", 1L), error = conditionMessage))
print(tryCatch(strtoi("1", 37L), error = conditionMessage))
print(tryCatch(strtoi(1, 10L), error = conditionMessage))
print(strtoi(character(0)))

print(utf8ToInt("aé€😀"))
print(utf8ToInt(""))
print(utf8ToInt(NA_character_))
print(withCallingHandlers(utf8ToInt(c("ab", "cd")), warning = function(w) {
  print(conditionMessage(w))
  invokeRestart("muffleWarning")
}))
print(tryCatch(utf8ToInt(character(0)), error = conditionMessage))
print(tryCatch(utf8ToInt(1L), error = conditionMessage))
print(utf8ToInt("\xff"))

print(charToRaw("aé€"))
print(charToRaw(""))
print(charToRaw(NA_character_))
print(withCallingHandlers(charToRaw(c("ab", "cd")), warning = function(w) {
  print(conditionMessage(w))
  invokeRestart("muffleWarning")
}))
print(tryCatch(charToRaw(character(0)), error = conditionMessage))
print(tryCatch(charToRaw(2), error = conditionMessage))

y <- c(a = "abcdef", b = NA, c = "xy", d = "héllo")
z <- y
substr(z, 2, 4) <- "ZZ"
print(z)
print(y)
z <- y
substr(z, 2, 4) <- c("123456", "q", NA, "ÄÖ")
print(z)
z <- y
substr(z, c(1, NA, 2, 5), 6) <- "++++"
print(z)
z <- y
substring(z, 3) <- "__"
print(z)
m <- matrix(c("aaaa", "bbbb", "cccc", "dddd"), 2, dimnames = list(c("r1", "r2"), NULL))
substr(m, 2, 3) <- c("XY", "Z")
print(m)
print(tryCatch({ n <- 1:3; substr(n, 1, 1) <- "a" }, error = conditionMessage))
print(tryCatch({ s <- "abc"; substr(s, 1, 1) <- 1 }, error = conditionMessage))
print(tryCatch({ s <- "abc"; substr(s, 1, 1) <- character(0) }, error = conditionMessage))
e <- character(0)
substr(e, 1, 1) <- "a"
print(e)

long <- as.character(1:200)
print(strrep(long, 2)[c(1, 64, 200)])
print(strtoi(long)[c(1, 64, 200)])
print(sum(strtoi(long)))
substr(long, 1, 1) <- "#"
print(long[c(1, 64, 65, 200)])
print(utf8ToInt(as.character(123:400)[1]))
f <- function(v) { substr(v, 2, 2) <- "!"; v }
for (i in 1:3) w <- f(as.character(100:400))
print(w[c(1, 64, 301)])
