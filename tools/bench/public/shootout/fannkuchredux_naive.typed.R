fannkuch <- function(n: I64) -> Record{checksum: I64, max_flips: I64} {
  max_flips: I64 <- 0
  checksum: I64 <- 0
  sign <- Bool.true
  perm1 <- List.range(1, n)
  count <- List.range(0, n - 1)
  while (Bool.true) {
    k <- perm1[[1]]
    perm <- perm1
    flips: I64 <- 1
    kk <- perm[[k]]
    while (kk > 1) {
      for (lo in List.range(1, k %/% 2)) perm <- List.swap(perm, lo, k - lo + 1)
      flips <- flips + 1
      k <- kk
      kk <- perm[[k]]
    }
    max_flips <- Num.max(max_flips, flips)
    checksum <- if (sign) checksum + flips else checksum - flips

    # The next permutation, by incremental change.
    if (sign) {
      perm1 <- List.swap(perm1, 1, 2)
      sign <- Bool.false
    } else {
      perm1 <- List.swap(perm1, 2, 3)
      sign <- Bool.true
      r <- 3
      found <- Bool.false
      while (!found && r <= n - 1) {
        if (count[[r]] != 0) {
          found <- Bool.true
        } else {
          count[[r]] <- r - 1
          perm0 <- perm1[[1]]
          for (i in List.range(1, r)) perm1[[i]] <- perm1[[i + 1]]
          perm1[[r + 1]] <- perm0
          r <- r + 1
        }
      }
      if (!found) {
        r <- n
        if (count[[r]] == 0)
          return((checksum = checksum, max_flips = max_flips))
      }
      count[[r]] <- count[[r]] - 1
    }
  }
  (checksum = checksum, max_flips = max_flips)
}

# ---------------------------------------------------------------------------
# Driver: the same as fannkuchredux_naive.R's, except that the typed program returns the
# checksum it prints and the driver prints it inside the clock.
local({
  started <- proc.time()[[3]]
  output <- capture.output({
    result <- fannkuch(9)
    cat(Num.to_r(result$checksum), "\n", sep = "")
  })
  seconds <- proc.time()[[3]] - started
  value <- Num.to_r(result$max_flips)
  text <- c(output, capture.output(str(value, vec.len = 3, digits.d = 10, give.attr = FALSE)))
  bytes <- utf8ToInt(paste(text, collapse = "\n"))
  checksum <- sprintf("%d:%.0f", length(bytes), sum(as.numeric(bytes) * (seq_along(bytes) %% 251 + 1)))
  cat(sprintf("fannkuchredux_naive %s %.4f\n", checksum, seconds))
})
