# Compiled code reads one element of a number vector converted to text, which converts only
# that element, and then calls a closure that is compiled at this call. Row subsetting of a
# data frame does both.
row <- function(frame) frame[2L, , drop = FALSE]
for (round in 1:4) print(row(data.frame(val = as.character(c(1L, 2L)))))
for (round in 1:4) print(row(data.frame(val = as.character(c(1.5, 2)), other = as.character(3:4))))

shout <- function(text) toupper(text)
read_then_call <- function(texts) {
  first <- texts[[1]]
  last <- texts[length(texts)]
  paste(shout(first), shout(last))
}
for (round in 1:4) print(read_then_call(as.character(c(10L, 20L, 30L) + round)))
for (round in 1:4) print(read_then_call(as.character(c(0.5, 1e10, 1e-10) * round)))

total <- 0L
for (index in 1:200) {
  texts <- as.character(index:(index + 2L))
  total <- total + nchar(texts[[2]]) + nchar(shout(texts[[3]]))
}
total
