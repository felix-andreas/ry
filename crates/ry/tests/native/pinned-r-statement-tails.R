# An R call whose value is unused, as the last statement of a loop body or of a branch. The
# checker accepts each, and the typed core does not compile them yet.
loop_tail <- function(n: I64) -> F64 {
  total <- 0f64
  for (i in List.range(1, n)) {
    total <- total + Num.to_f64(i)
    cat("step", total, "\n")
  }
  total
}
loop_statement <- function(n: I64) -> F64 {
  for (i in List.range(1, n)) cat("row", Num.to_r(i), "\n")
  1
}
while_tail <- function(n: I64) -> F64 {
  left <- n
  while (left > 0) {
    left <- left - 1
    cat("left", Num.to_r(left), "\n")
  }
  Num.to_f64(left)
}
branch_statement <- function(n: I64) -> F64 {
  if (n > 2) cat("large\n")
  1
}
both_branches <- function(n: I64) -> F64 {
  if (n > 2) {
    cat("large\n")
  } else {
    cat("small\n")
  }
  1
}
match_arms <- function(text: Str) -> F64 {
  match (Str.to_i64(text)) {
    :Ok(number) => cat("number", Num.to_r(number), "\n")
    :Err(_) => cat("not a number\n")
  }
  1
}
cat(loop_tail(2), loop_statement(2), while_tail(2), branch_statement(3), both_branches(3), match_arms("12"), "\n")
