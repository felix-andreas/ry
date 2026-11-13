# The 3 x 5 position and velocity matrices and the 5 x 5 x 3 difference array are flat
# column-major lists of F64, because typed ry has no matrices or arrays. The state that the R
# program updates with <<- is threaded through as a record.
alias State = Record{r: List<F64>, v: List<F64>}

execute <- function(n: I64) -> Record{before: F64, after: F64} {
  pi <- 3.141592653589793f64
  solar_mass <- 4 * pi * pi
  days_per_year <- 365.24f64

  r <- [
    # sun
    0f64, 0f64, 0f64,
    # jupiter
    4.84143144246472090e+00f64, -1.16032004402742839e+00f64, -1.03622044471123109e-01f64,
    # saturn
    8.34336671824457987e+00f64, 4.12479856412430479e+00f64, -4.03523417114321381e-01f64,
    # uranus
    1.28943695621391310e+01f64, -1.51111514016986312e+01f64, -2.23307578892655734e-01f64,
    # neptune
    1.53796971148509165e+01f64, -2.59193146099879641e+01f64, 1.79258772950371181e-01f64
  ]
  v <- [
    # sun
    0f64, 0f64, 0f64,
    # jupiter
    1.66007664274403694e-03f64 * days_per_year,
    7.69901118419740425e-03f64 * days_per_year,
    -6.90460016972063023e-05f64 * days_per_year,
    # saturn
    -2.76742510726862411e-03f64 * days_per_year,
    4.99852801234917238e-03f64 * days_per_year,
    2.30417297573763929e-05f64 * days_per_year,
    # uranus
    2.96460137564761618e-03f64 * days_per_year,
    2.37847173959480950e-03f64 * days_per_year,
    -2.96589568540237556e-05f64 * days_per_year,
    # neptune
    2.68067772490389322e-03f64 * days_per_year,
    1.62824170038242295e-03f64 * days_per_year,
    -9.51592254519715870e-05f64 * days_per_year
  ]
  mass <- [
    solar_mass,
    9.54791938424326609e-04f64 * solar_mass,
    2.85885980666130812e-04f64 * solar_mass,
    4.36624404335156298e-05f64 * solar_mass,
    5.15138902046611451e-05f64 * solar_mass
  ]

  # offset_momentum: the sun's velocity is -(body_v %*% body_mass) / solar_mass.
  momentum <- times_vector(v, 3, mass)
  for (d in List.range(1, 3)) v[[d]] <- 0 - momentum[[d]] / solar_mass

  state <- (r = r, v = v)
  before <- energy(state, mass)
  for (i in List.range(1, n))
    state <- advance(state, mass, 0.01)
  (before = before, after = energy(state, mass))
}

# m %*% x for a matrix m with `rows` rows, summing the columns scaled by x.
times_vector <- function(m: List<F64>, rows: I64, x: List<F64>) -> List<F64>
  List.walk(List.range(1, List.len(x)), List.repeat(0f64, rows), \(sum, j) sum + List.sublist(m, (j - 1) * rows + 1, rows) * x[[j]])

# drr[i, j, ] <- body_r[, i] - body_r[, j]
differences <- function(r: List<F64>) -> List<F64> {
  n_bodies <- 5i64
  drr <- List.repeat(0f64, n_bodies * n_bodies * 3)
  for (i in List.range(1, n_bodies)) {
    for (j in List.range(1, n_bodies)) {
      for (d in List.range(1, 3)) {
        drr[[(d - 1) * n_bodies * n_bodies + (j - 1) * n_bodies + i]] <- r[[(i - 1) * 3 + d]] - r[[(j - 1) * 3 + d]]
      }
    }
  }
  drr
}

# sqrt(t(colSums(aperm(drr * drr)))): the distance between bodies i and j at [i, j].
distances <- function(drr: List<F64>) -> List<F64> {
  squares <- drr * drr
  List.map(List.sublist(squares, 1, 25) + List.sublist(squares, 26, 25) + List.sublist(squares, 51, 25), \(square) Num.sqrt(square))
}

advance <- function(state: State, mass: List<F64>, dt: F64) -> State {
  n_bodies <- 5i64
  drr <- differences(state$r)
  distance <- distances(drr)
  mag <- List.map(distance, \(x) dt / (x * x * x))
  for (i in List.range(1, n_bodies)) mag[[(i - 1) * n_bodies + i]] <- 0f64
  v <- state$v
  for (d in List.range(1, 3)) {
    pull <- times_vector(List.sublist(drr, (d - 1) * 25 + 1, 25) * mag, n_bodies, mass)
    for (i in List.range(1, n_bodies)) v[[(i - 1) * 3 + d]] <- v[[(i - 1) * 3 + d]] - pull[[i]]
  }
  (r = state$r + v * dt, v = v)
}

energy <- function(state: State, mass: List<F64>) -> F64 {
  n_bodies <- 5i64
  distance <- distances(differences(state$r))
  squares <- state$v * state$v
  speeds <- List.map(List.range(1, n_bodies), \(i) List.sum(List.sublist(squares, (i - 1) * 3 + 1, 3)))
  kinetic <- List.sum(mass * 0.5 * speeds)
  potential <- 0f64
  for (j in List.range(1, n_bodies)) {
    if (j > 1) {
      for (i in List.range(1, j - 1))
        potential <- potential + mass[[i]] * mass[[j]] / distance[[(j - 1) * n_bodies + i]]
    }
  }
  kinetic - potential
}

# ---------------------------------------------------------------------------
# Driver: the same as nbody_2.R's, except that the typed program returns the two energies
# it prints and the driver prints them inside the clock.
local({
  started <- proc.time()[[3]]
  output <- capture.output({
    options(digits = 9)
    result <- execute(20000)
    cat(Num.to_r(result$before), "\n")
    value <- cat(Num.to_r(result$after), "\n")
  })
  seconds <- proc.time()[[3]] - started
  text <- c(output, capture.output(str(value, vec.len = 3, digits.d = 10, give.attr = FALSE)))
  bytes <- utf8ToInt(paste(text, collapse = "\n"))
  checksum <- sprintf("%d:%.0f", length(bytes), sum(as.numeric(bytes) * (seq_along(bytes) %% 251 + 1)))
  cat(sprintf("nbody_2 %s %.4f\n", checksum, seconds))
})
