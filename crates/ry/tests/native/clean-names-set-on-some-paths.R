alias Outcome = :Ok(I64) | :Err(Str)
last_over <- function(xs: List<I64>, limit: I64) -> I64 {
  for (x in xs) {
    if (x > limit) {
      found <- x
    }
  }
  found
}
counted <- function(n: I64) -> I64 {
  k <- 0
  while (k < n) {
    k <- k + 1
    seen <- k * 10
  }
  seen
}
payload <- function(r: Outcome) -> I64 {
  match (r) {
    :Ok(n) => 0
    :Err(_) => 1
  }
  n
}
relabel <- function(flag: Bool) -> Str {
  if (flag) {
    label <- 1
  }
  label <- "text"
  Str.concat(label, "!")
}
shifted <- function(xs: List<I64>) -> I64 {
  total <- 0
  for (x in xs) {
    add <- function(v: I64) -> I64 v + x
    total <- total + add(1)
  }
  total
}
both <- function(x: F64, up: Bool) -> F64 {
  if (up) {
    step <- function(v: F64) -> F64 v + 1
    y <- step(x)
  } else {
    step <- function(v: F64) -> F64 v - 1
    y <- step(x)
  }
  y
}
print(last_over([1, 5, 3], 4))
print(both(1, TRUE))
print(counted(3))
print(payload(:Ok(9)))
print(relabel(FALSE))
print(shifted([1, 2]))
