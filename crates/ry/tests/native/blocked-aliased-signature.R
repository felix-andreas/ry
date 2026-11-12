# blocked: aliased_in_a_comment, aliased_on_the_name
# A signature runs natively wherever its types are written: on the parameters, in the `#:` comment
# above the function, or on the name the function is assigned to. A type alias names a signature
# without writing it, which the runtime does not read.
#: @alias Pick {fn(n: I64, m: I64) -> I64}

#: Pick
aliased_in_a_comment <- function(n, m) n
aliased_on_the_name: Pick <- function(n, m) m
inline <- function(n: I64) -> I64 n * 2
#: fn(n: I64) -> I64
in_a_comment <- function(n) n * 2
#: @param n {I64}
#: @return {I64}
in_an_expanded_comment <- function(n) n * 2
on_the_name: fn(n: I64) -> I64 <- function(n) n * 2
#: fn(n: I64, m: I64) -> I64
mixed <- function(n, m: I64) n * m
#: (fn(I64) -> I64)
positional <- function(n) n * 2
cat(
  Num.to_r(aliased_in_a_comment(2, 3)),
  Num.to_r(aliased_on_the_name(2, 3)),
  Num.to_r(inline(2)),
  Num.to_r(in_a_comment(2)),
  Num.to_r(in_an_expanded_comment(2)),
  Num.to_r(on_the_name(2)),
  Num.to_r(mixed(2, 3)),
  Num.to_r(positional(2)),
  "\n"
)
