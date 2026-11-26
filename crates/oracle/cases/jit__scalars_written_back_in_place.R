# A compiled loop writes its unboxed variables back to the frame before a call that can read
# it. A write-back stores into the box the variable holds only when nothing else references
# that box, so a value that left the loop through a call or a list keeps what it held.

opaque <- function(...) 0
identity_through_dots <- function(value, ...) value

accumulate <- function(n) {
  total <- 0
  for (i in 1:n) {
    scaled <- i * 2.5
    shifted <- scaled + 1
    total <- total + shifted
    opaque()
  }
  c(total, scaled, shifted, i)
}
print(accumulate(5))
print(accumulate(40))

keeps_an_earlier_value <- function(n) {
  total <- 0
  kept <- NULL
  for (i in 1:n) {
    total <- total + 1.5
    if (i == 2L) kept <- identity_through_dots(total)
    opaque()
  }
  list(kept, total)
}
str(keeps_an_earlier_value(6))

collects_each_value <- function(n) {
  count <- 0L
  seen <- list()
  for (i in 1:n) {
    count <- count + 2L
    seen[[i]] <- identity_through_dots(count)
    opaque()
  }
  unlist(seen)
}
print(collects_each_value(6))

changes_type <- function(n) {
  value <- 1L
  flag <- TRUE
  for (i in 1:n) {
    opaque()
    value <- if (i %% 2L == 0L) value + 0.5 else value + 1L
    flag <- !flag
    opaque()
  }
  list(value, flag)
}
str(changes_type(7))

callee_reads_the_frame <- function(n) {
  total <- 0
  step <- 0L
  peek <- function(...) total * 10 + step
  out <- numeric(0)
  for (i in 1:n) {
    total <- total + 0.25
    step <- step + 1L
    out[i] <- peek()
  }
  out
}
print(callee_reads_the_frame(5))

formal_is_assigned <- function(n, start, scale = 2) {
  before <- c(missing(start), missing(scale))
  start <- 0
  for (i in 1:n) {
    start <- start + scale
    scale <- scale + 0.5
    opaque()
  }
  c(before, missing(start), missing(scale), start, scale)
}
print(formal_is_assigned(4))
print(formal_is_assigned(4, 10, 1))

reads_through_the_environment <- function(n) {
  total <- 0
  for (i in 1:n) {
    total <- total + i / 2
    opaque()
  }
  environment()
}
frame <- reads_through_the_environment(8)
print(c(frame$total, frame$i))

many_variables <- function(n) {
  v1 <- 0; v2 <- 0; v3 <- 0; v4 <- 0; v5 <- 0; v6 <- 0; v7 <- 0
  count <- 0L
  for (i in 1:n) {
    v1 <- v1 + 1; v2 <- v2 + v1; v3 <- v3 + v2; v4 <- v4 + v3
    v5 <- v5 + v4; v6 <- v6 + v5; v7 <- v7 + v6
    count <- count + 1L
    opaque()
  }
  c(v1, v2, v3, v4, v5, v6, v7, count)
}
print(many_variables(6))

frame_becomes_an_environment <- function(n) {
  total <- 0
  frame <- NULL
  for (i in 1:n) {
    total <- total + 0.5
    if (i == 3L) frame <- environment()
    opaque()
  }
  c(total, frame$total, get("total", frame))
}
print(frame_becomes_an_environment(8))

# A call through `do.call` can enter the body with an environment, where the frame has no
# slots to store into. The unused formals put `total` far into the slot table.
entered_with_an_environment <- function(a1, a2, a3, a4, a5, a6, a7, a8, a9, a10, a11, a12,
                                        a13, a14, a15, a16, a17, a18, a19, a20, a21, a22,
                                        a23, a24, a25, a26, a27, a28, a29, a30, n) {
  total <- 0
  for (i in 1:n) {
    total <- total + i
    opaque()
  }
  total
}
print(do.call(entered_with_an_environment, c(as.list(1:30), 10L)))
print(do.call(entered_with_an_environment, c(as.list(1:30), 10L), quote = TRUE))

replaces_a_shaped_value <- function(n) {
  named <- c(a = 1)
  pair <- c(1, 2)
  for (i in 1:n) {
    named <- i * 2.5
    pair <- i + 0.5
    opaque()
  }
  list(names(named), named, length(pair), pair)
}
str(replaces_a_shaped_value(4))

computed_default <- function(n, scale = 1 + 1) {
  before <- missing(scale)
  force(scale)
  for (i in 1:n) {
    scale <- scale + 0.5
    opaque()
  }
  c(before, missing(scale), scale)
}
print(computed_default(4))
print(computed_default(4, 3))

six_variables <- function(n) {
  v1 <- 0; v2 <- 0; v3 <- 0; v4 <- 0; v5 <- 0
  count <- 0L
  for (i in 1:n) {
    v1 <- v1 + 1; v2 <- v2 + v1; v3 <- v3 + v2; v4 <- v4 + v3; v5 <- v5 + v4
    count <- count + 1L
    opaque()
  }
  c(v1, v2, v3, v4, v5, count)
}
print(six_variables(6))

for (repeat_index in 1:300) {
  do.call(entered_with_an_environment, c(as.list(1:30), 3L))
  replaces_a_shaped_value(3)
  computed_default(2)
  six_variables(3)
  callee_reads_the_frame(3)
  many_variables(3)
  frame_becomes_an_environment(4)
  accumulate(3)
  keeps_an_earlier_value(4)
  changes_type(3)
  formal_is_assigned(2)
}
print(accumulate(12))
str(keeps_an_earlier_value(9))
str(changes_type(8))
print(formal_is_assigned(3))
print(many_variables(9))
print(frame_becomes_an_environment(5))
print(do.call(entered_with_an_environment, c(as.list(1:30), 12L)))
str(replaces_a_shaped_value(5))
print(computed_default(3))
print(six_variables(7))
print(callee_reads_the_frame(4))
