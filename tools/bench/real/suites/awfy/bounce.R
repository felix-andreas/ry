source('random.r')

execute <- function (ballCount = 3000L) {
    bounces   <- 0
    balls     = vector("list", length = ballCount)
    resetSeed()

    for (i in 1:ballCount) {
        balls[[i]] = c(nextRandom() %% 500, nextRandom() %% 500,
                             (nextRandom() %% 300) - 150, (nextRandom() %% 300) - 150)
                      names(balls[[i]]) = c("x", "y", "xVel", "yVel")
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

    ball['x'] <- ball['x'] + ball['xVel'];
    ball['y'] <- ball['y'] + ball['yVel'];
    
    if (ball['x'] > xLimit) {
        ball['x'] <- xLimit 
        ball['xVel'] <- 0 - abs(ball['xVel']) 
        bounced <- TRUE
    } 
    if (ball['x'] < 0) {
        ball['x'] <- 0
        ball['xVel'] <- abs(ball['xVel'])
        bounced <- TRUE
    }
    if (ball['y'] > yLimit) {
        ball['y'] <- yLimit
        ball['yVel'] <- 0 - abs(ball['yVel'])
        bounced <- TRUE
    }
    if (ball['y'] < 0) {
        ball['y'] <- 0 
        ball['yVel'] <- abs(ball['yVel'])
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
  output <- capture.output(value <- execute(3000L))
  seconds <- proc.time()[[3]] - started
  text <- c(output, capture.output(str(value, vec.len = 3, digits.d = 10, give.attr = FALSE)))
  bytes <- utf8ToInt(paste(text, collapse = "\n"))
  checksum <- sprintf("%d:%.0f", length(bytes), sum(as.numeric(bytes) * (seq_along(bytes) %% 251 + 1)))
  cat(sprintf("bounce %s %.4f\n", checksum, seconds))
})
