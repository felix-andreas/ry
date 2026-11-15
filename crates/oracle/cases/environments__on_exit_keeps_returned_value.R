# Exit code that modifies a variable holding the value on its way to the caller writes to a
# copy: the caller receives the value the function returned.
x <- c(10L, 20L, 30L)
global_update <- function() {
  on.exit(x[1] <<- 0L)
  x
}
print(global_update())
print(x)

local_update <- function() {
  values <- c(1, 2, 3)
  on.exit(values[2] <- -1)
  values
}
print(local_update())

explicit_return <- function() {
  values <- list(a = 1, b = "two")
  on.exit(values$a <- NULL)
  return(values)
}
str(explicit_return())

helper <- function() x[2] <<- 99L
through_a_call <- function() {
  on.exit(helper())
  invisible(x)
}
print(through_a_call())
print(x)

several <- function() {
  values <- 1:4
  on.exit(values[1] <- 100L)
  on.exit(values[4] <- 400L, add = TRUE)
  on.exit(cat("at exit:", values, "\n"), add = TRUE)
  values
}
print(several())

replaced <- function() {
  values <- c(5, 6)
  on.exit({ values[1] <- 0; return(values) })
  values
}
print(replaced())

loop_total <- 0
for (step in 1:3) {
  counted <- function() {
    kept <- c(step, step * 2)
    on.exit(kept[1] <- NA)
    kept
  }
  loop_total <- loop_total + sum(counted())
}
print(loop_total)
