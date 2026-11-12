# blocked: area_of
area <- function(width: F64, height: F64) -> F64 width * height
area_of <- function(side: F64) -> F64 area(width = side, height = 2 * side)
cat(area_of(3), "\n")
