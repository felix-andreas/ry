# blocked: in_a_comment
# Parameter types written apart from the function literal, which the runtime does not read. A
# result type in a comment, or none at all, beside inline parameter types still compiles.
#: fn(n: I64) -> I64
in_a_comment <- function(n) n * 2
inline <- function(n: I64) -> I64 n * 2
#: I64
result_in_a_comment <- function(n: I64) n * 2
no_result <- function(n: I64) n * 2
cat(
  Num.to_r(in_a_comment(2)),
  Num.to_r(inline(2)),
  Num.to_r(result_in_a_comment(2)),
  Num.to_r(no_result(2)),
  "\n"
)
