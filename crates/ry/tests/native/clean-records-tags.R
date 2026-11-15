# Records, tuples, tags, nominal types and `match`.
alias Point = Record{x: F64, y: F64}
type Shape = :Circle(center: Point, radius: F64) | :Rect(corner: Point, width: F64, height: F64) | :Dot
alias Light = :Red | :Amber | :Green

area <- function(shape: Shape) -> F64 {
  match (shape) {
    :Circle(center = _, radius = r) => Num.pi * r * r
    :Rect(corner = _, width = w, height = h) => w * h
    :Dot => 0
  }
}
moved <- function(point: Point, dx: F64) -> Point (..point, x = point$x + dx)
next_light <- function(light: Light) -> Light {
  match (light) {
    :Red => :Green
    :Amber => :Red
    :Green => :Amber
  }
}
bounds <- function(values: List<F64>) -> Tuple{F64, F64} {
  low <- values[[1]]
  high <- values[[1]]
  for (value in values) {
    low <- Num.min(low, value)
    high <- Num.max(high, value)
  }
  (low, high)
}
total_area <- function(n: I64) -> F64 {
  origin: Point <- (x = 0, y = 0)
  total <- 0
  light <- next_light(next_light(:Green))
  for (i in List.range(1, n)) {
    side <- Num.to_f64(i)
    shape <- if (i %% 3 == 0) @Shape(:Dot) else if (i %% 3 == 1) @Shape(:Circle(center = origin, radius = side)) else @Shape(:Rect(corner = moved(origin, side), width = side, height = 2f64))
    total <- total + area(shape)
  }
  range <- bounds([total, 1, 2 * total])
  weight: F64 <- match (light) {
    :Red => 1f64
    :Amber => 2f64
    :Green => 3f64
  }
  range[[2]] - range[[1]] + weight
}
cat(total_area(9), "\n")
