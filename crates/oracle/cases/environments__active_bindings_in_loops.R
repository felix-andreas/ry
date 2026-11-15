# Reading an active binding calls its function every time, including in a loop hot enough to
# compile. GNU R's byte code may call the function more than once per read, so the checks hold
# for any count of at least one call per read: each read must see a fresh value.

reads <- 0L
makeActiveBinding("counted", function() {
  reads <<- reads + 1L
  reads
}, globalenv())

summed <- function(n) {
  s <- counted
  for (i in seq_len(n)) s <- s + counted
  s
}
total <- summed(2000L)
print(reads >= 2001L)
# The values read are increasing positive integers, so 2001 fresh reads sum to at least
# 1 + 2 + ... + 2001; a reused value falls short.
print(total >= 2001 * 2002 / 2)

collected <- function(n) {
  seen <- integer(n)
  for (i in seq_len(n)) seen[i] <- counted
  seen
}
seen <- collected(3000L)
print(length(seen))
print(all(diff(seen) > 0L))

# A super-assignment calls the binding's function with the value, and the next read sees what
# that call stored.
stored <- 0
makeActiveBinding("doubled", function(value) {
  if (missing(value)) stored else stored <<- 2 * value
}, globalenv())
assigned <- function(n) {
  s <- 0
  for (i in seq_len(n)) {
    doubled <<- i
    s <- s + doubled
  }
  s
}
print(assigned(3000L))
print(doubled)

# The same function called again keeps calling the binding.
before <- reads
again <- summed(10L)
print(reads - before >= 11L)
print(again >= 11 * before + 66)
