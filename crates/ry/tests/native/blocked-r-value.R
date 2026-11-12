# blocked: label_width
# The value of an R call reaches ry code without a conversion.
label_width <- function(label: Str) -> F64 {
  width <- nchar(label)
  Num.to_f64(Num.from_r(width)) * 2
}
cat(label_width("native"), "\n")
