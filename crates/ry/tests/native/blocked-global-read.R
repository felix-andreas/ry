# blocked: shifted
alias Point = Record{x: F64, y: F64}
origin: Point <- (x = 1, y = 2)
shifted <- function(dx: F64) -> F64 origin$x + dx
cat(shifted(1), "\n")
