execute <- function (unused) {
    seed <- NaN

    resetSeed <- function() seed <<- 74755

    nextRandom <- function() {
      seed <<- bitwAnd((seed * 1309) + 13849, 65535)
      return (seed)
    }

    ballCount <- 10000
    bounces   <- 0
    balls     = vector("list", length = ballCount)
    resetSeed()

    for (i in 1:ballCount) {
        balls[[i]] = c(nextRandom() %% 500, nextRandom() %% 500, 
                             (nextRandom() %% 300) - 150, (nextRandom() %% 300) - 150)
    }

    ball <- function(ball) {
        results <- bounce(ball)
        if (results[[2]]) bounces <<- bounces + 1
        return (results[[1]])
    }

    for (i in 1:50) balls <- lapply(balls, ball)
    return (bounces)
}

bounce <- function(ball) {
    xLimit  <- 500
    yLimit  <- 500
    bounced <- FALSE

    ball[1] <- ball[1] + ball[3];
    ball[2] <- ball[2] + ball[4];

    if (ball[1] > xLimit) {
        ball[1] <- xLimit 
        ball[3] <- 0 - abs(ball[3]) 
        bounced <- TRUE
    }
    if (ball[1] < 0) {
        ball[1] <- 0
        ball[3] <- abs(ball[3])
        bounced <- TRUE
    }
    if (ball[2] > yLimit) {
        ball[2] <- yLimit
        ball[4] <- 0 - abs(ball[4])
        bounced <- TRUE
    }
    if (ball[2] < 0) {
        ball[2] <- 0 
        ball[4] <- abs(ball[4])
        bounced <- TRUE
    }
    return (list(ball, bounced))
}

# ---------------------------------------------------------------------------
# Driver added for ry's `just bench-real --public` (not part of the upstream
# file). It runs `execute` once at a fixed size and prints one line:
#   <name> <checksum of printed output and result> <seconds>
# Run it from this directory: upstream code reads sibling files by relative path.
local({
  started <- proc.time()[[3]]
  output <- capture.output(value <- execute(NULL))
  seconds <- proc.time()[[3]] - started
  text <- c(output, capture.output(str(value, vec.len = 3, digits.d = 10, give.attr = FALSE)))
  bytes <- utf8ToInt(paste(text, collapse = "\n"))
  checksum <- sprintf("%d:%.0f", length(bytes), sum(as.numeric(bytes) * (seq_along(bytes) %% 251 + 1)))
  cat(sprintf("bounce_nonames %s %.4f\n", checksum, seconds))
})
