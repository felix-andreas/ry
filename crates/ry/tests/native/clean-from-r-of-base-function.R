# A conversion of what a base R function returns, where the typed core computes the function
# itself, and a `sprintf` format R formats.
label_width <- function(label: Str) -> I64 Num.from_r(nchar(label))
shouted <- function(label: Str) -> Str Str.from_r(toupper(label))
root <- function(x: F64) -> F64 Num.from_r(sqrt(x))
framed <- function(label: Str) -> Str Str.from_r(sprintf("[%s]", label))
cat(Num.to_r(label_width("native")), shouted("native"), root(16), framed("native"), "\n")
