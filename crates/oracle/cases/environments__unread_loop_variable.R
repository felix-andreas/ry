# A loop variable the body never reads is bound like any other: after the loop, through the
# frame's environment, and by name.
run <- function(n) {
  total <- 0
  for (k in seq_len(n)) total <- total + 1
  for (unused in c("a", "b")) NULL
  list(total, k, unused, exists("k", inherits = FALSE), sort(ls()), get("unused"))
}
for (round in 1:3) str(run(4L))
seen <- function() { for (k in 1:3) NULL; environment() }
for (round in 1:3) print(get("k", envir = seen()))
