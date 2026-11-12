alias Body = Record{x: F64, y: F64, z: F64, vx: F64, vy: F64, vz: F64, mass: F64}

execute <- function(n: I64) -> Record{before: F64, after: F64} {
  pi <- 3.141592653589793f64
  solar_mass <- 4 * pi * pi
  days_per_year <- 365.24f64

  bodies <- [
    # sun
    (x = 0f64, y = 0f64, z = 0f64, vx = 0f64, vy = 0f64, vz = 0f64, mass = solar_mass),
    # jupiter
    (
      x = 4.84143144246472090e+00f64,
      y = -1.16032004402742839e+00f64,
      z = -1.03622044471123109e-01f64,
      vx = 1.66007664274403694e-03f64 * days_per_year,
      vy = 7.69901118419740425e-03f64 * days_per_year,
      vz = -6.90460016972063023e-05f64 * days_per_year,
      mass = 9.54791938424326609e-04f64 * solar_mass
    ),
    # saturn
    (
      x = 8.34336671824457987e+00f64,
      y = 4.12479856412430479e+00f64,
      z = -4.03523417114321381e-01f64,
      vx = -2.76742510726862411e-03f64 * days_per_year,
      vy = 4.99852801234917238e-03f64 * days_per_year,
      vz = 2.30417297573763929e-05f64 * days_per_year,
      mass = 2.85885980666130812e-04f64 * solar_mass
    ),
    # uranus
    (
      x = 1.28943695621391310e+01f64,
      y = -1.51111514016986312e+01f64,
      z = -2.23307578892655734e-01f64,
      vx = 2.96460137564761618e-03f64 * days_per_year,
      vy = 2.37847173959480950e-03f64 * days_per_year,
      vz = -2.96589568540237556e-05f64 * days_per_year,
      mass = 4.36624404335156298e-05f64 * solar_mass
    ),
    # neptune
    (
      x = 1.53796971148509165e+01f64,
      y = -2.59193146099879641e+01f64,
      z = 1.79258772950371181e-01f64,
      vx = 2.68067772490389322e-03f64 * days_per_year,
      vy = 1.62824170038242295e-03f64 * days_per_year,
      vz = -9.51592254519715870e-05f64 * days_per_year,
      mass = 5.15138902046611451e-05f64 * solar_mass
    )
  ]

  bodies <- offset_momentum(bodies, solar_mass)
  before <- energy(bodies)
  for (step in List.range(1, n))
    bodies <- advance(bodies, 0.01)
  (before = before, after = energy(bodies))
}

offset_momentum <- function(bodies: List<Body>, solar_mass: F64) -> List<Body> {
  px <- List.sum(List.map(bodies, \(b) b$vx * b$mass))
  py <- List.sum(List.map(bodies, \(b) b$vy * b$mass))
  pz <- List.sum(List.map(bodies, \(b) b$vz * b$mass))
  sun <- bodies[[1]]
  bodies[[1]] <- (..sun, vx = -px / solar_mass, vy = -py / solar_mass, vz = -pz / solar_mass)
  bodies
}

advance <- function(bodies: List<Body>, dt: F64) -> List<Body> {
  count <- List.len(bodies)
  moved <- bodies
  for (i in List.range(1, count)) {
    a <- bodies[[i]]
    ax: F64 <- 0
    ay: F64 <- 0
    az: F64 <- 0
    for (j in List.range(1, count)) {
      if (j != i) {
        b <- bodies[[j]]
        dx <- a$x - b$x
        dy <- a$y - b$y
        dz <- a$z - b$z
        distance <- Num.sqrt(dx * dx + dy * dy + dz * dz)
        mag <- dt / (distance * distance * distance)
        ax <- ax + dx * mag * b$mass
        ay <- ay + dy * mag * b$mass
        az <- az + dz * mag * b$mass
      }
    }
    vx <- a$vx - ax
    vy <- a$vy - ay
    vz <- a$vz - az
    moved[[i]] <- (..a, x = a$x + dt * vx, y = a$y + dt * vy, z = a$z + dt * vz, vx = vx, vy = vy, vz = vz)
  }
  moved
}

energy <- function(bodies: List<Body>) -> F64 {
  count <- List.len(bodies)
  kinetic <- List.sum(List.map(bodies, \(b) 0.5 * b$mass * (b$vx * b$vx + b$vy * b$vy + b$vz * b$vz)))
  potential: F64 <- 0
  for (j in List.range(1, count)) {
    for (i in List.range(1, j - 1)) {
      a <- bodies[[i]]
      b <- bodies[[j]]
      dx <- a$x - b$x
      dy <- a$y - b$y
      dz <- a$z - b$z
      potential <- potential + a$mass * b$mass / Num.sqrt(dx * dx + dy * dy + dz * dz)
    }
  }
  kinetic - potential
}

# ---------------------------------------------------------------------------
# Driver: the same as nbody.R's, except that the typed program returns the two energies
# it prints and the driver prints them inside the clock.
local({
  started <- proc.time()[[3]]
  output <- capture.output({
    options(digits = 9)
    result <- execute(40000)
    cat(result$before, "\n")
    value <- cat(result$after, "\n")
  })
  seconds <- proc.time()[[3]] - started
  text <- c(output, capture.output(str(value, vec.len = 3, digits.d = 10, give.attr = FALSE)))
  bytes <- utf8ToInt(paste(text, collapse = "\n"))
  checksum <- sprintf("%d:%.0f", length(bytes), sum(as.numeric(bytes) * (seq_along(bytes) %% 251 + 1)))
  cat(sprintf("nbody %s %.4f\n", checksum, seconds))
})
