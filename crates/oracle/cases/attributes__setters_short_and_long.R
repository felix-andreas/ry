# Each attribute setter on a shared vector below and above 64 elements, where the copy that
# carries the new attributes is an ALTREP wrapper over the original's elements.
for (n in c(0L, 5L, 63L, 64L, 100L)) {
  x <- as.numeric(seq_len(n))
  shared <- x

  y <- x
  names(y) <- sprintf("e%d", seq_len(n))
  stopifnot(is.null(names(x)))
  print(c(n = n, named = length(names(y)), last = if (n) tail(names(y), 1) else NA))

  y <- x
  if (n) y[1] <- 0
  print(c(head(y, 2), head(x, 2)))

  y <- x
  attr(y, "tag") <- "t"
  print(attributes(y))
  print(is.null(attr(x, "tag")))

  y <- x
  class(y) <- "thing"
  print(class(y))
  print(inherits(unclass(y), "thing"))
  print(oldClass(x))

  y <- x
  comment(y) <- "note"
  print(comment(y))
  comment(y) <- character()
  print(comment(y))

  if (n %% 2 == 0 && n > 0) {
    y <- x
    dim(y) <- c(2L, n / 2)
    print(dim(y))
    print(is.null(dim(x)))
    dimnames(y) <- list(c("a", "b"), NULL)
    print(dimnames(y))
    dimnames(y) <- list(NULL, NULL)
    print(dimnames(y))
  }

  y <- x
  attributes(y) <- list(names = as.character(seq_len(n)), extra = 1:2)
  print(names(attributes(y)))
  attributes(y) <- NULL
  print(attributes(y))

  y <- x
  length(y) <- n + 3L
  print(tail(y, 4))
  z <- setNames(x, seq_len(n))
  length(z) <- max(n - 2L, 0L)
  print(tail(names(z), 2))
  length(z) <- n
  print(tail(names(z), 3))
  print(identical(x, shared))
}

# NA, empty and factor inputs.
x <- c(a = 1, b = NA, c = 3)
names(x)[2] <- NA
print(names(x))
names(x) <- NULL
print(x)
f <- factor(c("lo", "hi", NA, "lo"))
levels(f) <- c("high", "low")
print(f)
levels(f) <- list(H = "high", L = "low")
print(f)
print(tryCatch({ levels(f) <- c("a", "a"); f }, warning = conditionMessage, error = conditionMessage))
long <- factor(rep(c("u", "v"), 50))
other <- long
levels(other) <- c("U", "V")
print(table(long))
print(table(other))
e <- character()
length(e) <- 2
print(e)
l <- list(1, "a")
length(l) <- 4
str(l)
names(l) <- c("p", "q")
print(names(l))
print(tryCatch(dim(x) <- c(2, 2), error = conditionMessage))
print(tryCatch(dim(x) <- c(NA, 3), error = conditionMessage))
print(tryCatch(dimnames(x) <- list("a"), error = conditionMessage))
print(tryCatch(length(x) <- NA, error = conditionMessage))
print(tryCatch(length(x) <- 1:2, error = conditionMessage))
m <- matrix(1:6, 2)
print(row(m))
print(col(m))
print(tryCatch(row(1:3), error = conditionMessage))
print(.Internal(islistfactor(list(f, f), FALSE)))
print(.Internal(islistfactor(list(list(f), f), TRUE)))
print(.Internal(islistfactor(list(list(f), f), FALSE)))
print(.Internal(islistfactor(list(), FALSE)))
print(.Internal(islistfactor(list(1), FALSE)))
d <- data.frame(a = 1:3)
print(.row_names_info(d))
print(.row_names_info(d, 1L))
print(.row_names_info(d, 0L))
d2 <- data.frame(a = 1:2, row.names = c("r", "s"))
print(.row_names_info(d2))
print(.row_names_info(d2, 0L))

# A vector nothing else holds is changed where it is, at both sizes.
for (n in c(6L, 100L)) {
  own <- as.numeric(seq_len(n))
  names(own) <- sprintf("e%d", seq_len(n))
  attr(own, "tag") <- "t"
  class(own) <- "thing"
  own <- unclass(own)
  dim(own) <- c(2L, n / 2)
  dimnames(own) <- list(c("a", "b"), NULL)
  print(names(attributes(own)))
  print(own[2, n / 2])
  length(own) <- n + 1L
  print(tail(own, 2))
}

# Levels repeat when their text does, whatever the encoding mark, and for any vector type.
latin <- "caf\xe9"
Encoding(latin) <- "latin1"
codes <- 1:2
print(tryCatch({ levels(codes) <- c(latin, enc2utf8(latin)); "accepted" }, error = conditionMessage))
print(tryCatch({ levels(codes) <- c(NA, NA); "accepted" }, error = conditionMessage))
print(tryCatch({ levels(codes) <- c(2, 3, 2); "accepted" }, error = conditionMessage))
print(tryCatch({ levels(codes) <- c("u", "v"); "accepted" }, error = conditionMessage))
print(levels(codes))

# Compact row names read back as a sequence, and long explicit ones as they are.
frame <- data.frame(v = 1:200)
print(sum(attr(frame, "row.names")))
print(.row_names_info(frame))
attr(frame, "row.names") <- 200:1
print(head(attributes(frame)$row.names, 3))
