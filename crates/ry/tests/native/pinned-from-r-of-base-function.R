# A conversion of what a base R function returns, where the typed core computes the function
# itself. The checker accepts each, and the typed core does not compile them yet.
label_width <- function(label: Str) -> I64 Num.from_r(nchar(label))
shouted <- function(label: Str) -> Str Str.from_r(toupper(label))
root <- function(x: F64) -> F64 Num.from_r(sqrt(x))
cat(Num.to_r(label_width("native")), shouted("native"), root(16), "\n")
