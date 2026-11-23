# `==` and `!=` on character vectors, in a function, which runs compiled: each comparison runs
# several times, so the first run of a site and its later ones are both covered.
thrice <- function(f) for (i in 1:3) print(f())
words <- c("alpha", "beta", NA, "", "beta")
other <- c("alpha", "gamma", "beta", NA, "beta")

cat("-- two strings\n")
thrice(function() "a" == "a")
thrice(function() "a" == "b")
thrice(function() "a" != "b")
thrice(function() "a" != "a")
thrice(function() "" == "")
thrice(function() NA_character_ == "a")
thrice(function() "a" != NA_character_)
thrice(function() NA_character_ == NA_character_)
thrice(function() "NA" == NA_character_)
thrice(function() words[2L] == "beta")
thrice(function() paste0("be", "ta") == words[2L])

cat("-- a vector and a string, and two vectors\n")
thrice(function() words == "beta")
thrice(function() "beta" != words)
thrice(function() words == other)
thrice(function() words != other)
thrice(function() words == words)
thrice(function() typeof(words == other))

cat("-- an `if` chain on a string\n")
pick <- function(key) {
    if (key == "a") {
        1L
    } else if (key == "b") {
        2L
    } else if (key != "c") {
        3L
    } else {
        4L
    }
}
for (key in c("a", "b", "c", "d", "a")) print(pick(key))
print(tryCatch(pick(NA_character_), error = conditionMessage))
print(tryCatch(pick(character(0)), error = conditionMessage))

cat("-- lengths that recycle, and none\n")
thrice(function() c("a", "b", "a", "b") == c("a", "b"))
thrice(function() withCallingHandlers(
    c("a", "b", "a") == c("a", "b"),
    warning = function(w) {
        cat("warning:", conditionMessage(w), "\n")
        invokeRestart("muffleWarning")
    }
))
thrice(function() character(0) == "a")
thrice(function() "a" != character(0))
thrice(function() character(0) == character(0))

cat("-- attributes stay with the builtin\n")
named <- c(first = "a", second = "b")
thrice(function() named == "a")
thrice(function() "b" != named)
thrice(function() named == c(x = "a", y = "b"))
grid <- matrix(c("a", "b", "c", "a"), 2L)
thrice(function() grid == "a")
thrice(function() grid != t(grid))
tagged <- structure(c("a", "b"), unit = "m")
thrice(function() tagged == "a")
levels_of <- factor(c("a", "b", "a"))
thrice(function() levels_of == "a")
thrice(function() "a" != levels_of)
record <- structure("a", class = "label")
thrice(function() record == "a")
`==.label` <- function(e1, e2) "from the method"
thrice(function() record == "a")
thrice(function() "a" == record)
rm(`==.label`)

cat("-- encodings: the same text under two marks is equal, bytes only equal bytes\n")
latin <- "caf\xe9"
Encoding(latin) <- "latin1"
utf <- "café"
raw_bytes <- "caf\xe9"
Encoding(raw_bytes) <- "bytes"
plain_bytes <- "cafe"
Encoding(plain_bytes) <- "bytes"
thrice(function() latin == utf)
thrice(function() utf != latin)
thrice(function() c(latin, "x", utf) == c(utf, "x", latin))
thrice(function() raw_bytes == raw_bytes)
thrice(function() plain_bytes == "cafe")
thrice(function() c(utf, latin, NA) == "café")
thrice(function() enc2native(utf) == utf)

cat("-- an ordering comparison collates\n")
thrice(function() "a" < "b")
thrice(function() words <= "beta")
thrice(function() c("b", "a") > c("a", "b"))

cat("-- a string against another type\n")
thrice(function() "1" == 1)
thrice(function() 1L != "1")
thrice(function() "TRUE" == TRUE)
thrice(function() c("a", "1") == c(1, 1))
thrice(function() tryCatch("a" == quote(a), error = conditionMessage))
thrice(function() tryCatch("a" == list("a", 1), error = conditionMessage))

cat("-- the result is a new value each time\n")
changes <- function() {
    same <- words == "beta"
    same[1L] <- TRUE
    c(same, words == "beta")
}
thrice(changes)
