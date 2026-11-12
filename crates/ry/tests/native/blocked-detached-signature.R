# blocked: in_a_comment
# A signature written apart from the function literal, which the runtime does not read.
#: fn(n: I64) -> I64
in_a_comment <- function(n) n * 2
inline <- function(n: I64) -> I64 n * 2
cat(Num.to_r(in_a_comment(2)), Num.to_r(inline(2)), "\n")
