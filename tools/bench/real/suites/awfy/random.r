seed <- NaN

resetSeed <- function() seed <<- 74755

nextRandom <- function() {
  seed <<- bitwAnd((seed * 1309) + 13849, 65535)
  return (seed)
}
