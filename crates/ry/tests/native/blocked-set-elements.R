# blocked: spread
spread <- function(xs: List<F64>) -> I64 Set.len(Set.from_list(xs))
cat(Num.to_r(spread([1.5, 2.5, 1.5])), "\n")
