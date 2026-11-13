# A matrix is a flat column-major List<F64>, because typed ry has no matrices. typed ry has no
# eigen(), so the port computes the eigenvalues of the symmetric t(m) %*% m the way LAPACK's
# symmetric solvers start: a Householder reduction to tridiagonal form, then the implicit QL
# method on the tridiagonal matrix (tred2 and tqli, eigenvalues only).
execute <- function(n: I64) -> F64 {
  m <- List.join_map(List.range(1, n), \(j) List.map(List.range(1, n), \(i) eval_a(i, j)))
  # t(m) %*% m: entry (i, j) is the dot product of columns i and j.
  columns <- List.map(List.range(1, n), \(j) List.sublist(m, (j - 1) * n + 1, n))
  mtm <- List.join_map(columns, \(cj) List.map(columns, \(ci) List.sum(ci * cj)))
  Num.sqrt(List.max(eigenvalues(mtm, n)) |> Result.with_default(0f64))
}

eval_a <- function(i: I64, j: I64) -> F64
  1 / Num.to_f64(((i + j - 2) * (i + j - 1)) %/% 2 + i)

eigenvalues <- function(matrix: List<F64>, n: I64) -> List<F64> {
  # a[[i, j]] is a[[(j - 1) * n + i]]; the matrix is symmetric, so the layout does not matter.
  a <- matrix
  d <- List.repeat(0f64, n)
  e <- List.repeat(0f64, n)
  i <- n
  while (i >= 2) {
    l <- i - 1
    h <- 0f64
    if (l > 1) {
      scale <- 0f64
      for (k in List.range(1, l)) scale <- scale + Num.abs(a[[(k - 1) * n + i]])
      if (scale == 0) {
        e[[i]] <- a[[(l - 1) * n + i]]
      } else {
        for (k in List.range(1, l)) {
          a[[(k - 1) * n + i]] <- a[[(k - 1) * n + i]] / scale
          h <- h + a[[(k - 1) * n + i]] * a[[(k - 1) * n + i]]
        }
        f <- a[[(l - 1) * n + i]]
        g <- if (f >= 0) 0 - Num.sqrt(h) else Num.sqrt(h)
        e[[i]] <- scale * g
        h <- h - f * g
        a[[(l - 1) * n + i]] <- f - g
        f <- 0f64
        for (j in List.range(1, l)) {
          g <- 0f64
          for (k in List.range(1, j)) g <- g + a[[(k - 1) * n + j]] * a[[(k - 1) * n + i]]
          if (l > j) for (k in List.range(j + 1, l)) g <- g + a[[(j - 1) * n + k]] * a[[(k - 1) * n + i]]
          e[[j]] <- g / h
          f <- f + e[[j]] * a[[(j - 1) * n + i]]
        }
        hh <- f / (h + h)
        for (j in List.range(1, l)) {
          f <- a[[(j - 1) * n + i]]
          g <- e[[j]] - hh * f
          e[[j]] <- g
          for (k in List.range(1, j)) a[[(k - 1) * n + j]] <- a[[(k - 1) * n + j]] - (f * e[[k]] + g * a[[(k - 1) * n + i]])
        }
      }
    } else {
      e[[i]] <- a[[(l - 1) * n + i]]
    }
    d[[i]] <- h
    i <- i - 1
  }
  for (i in List.range(1, n)) d[[i]] <- a[[(i - 1) * n + i]]

  for (i in List.range(2, n)) e[[i - 1]] <- e[[i]]
  e[[n]] <- 0f64
  for (l in List.range(1, n)) {
    searching <- Bool.true
    while (searching) {
      m <- l
      while (m < n && Num.abs(e[[m]]) + (Num.abs(d[[m]]) + Num.abs(d[[m + 1]])) != Num.abs(d[[m]]) + Num.abs(d[[m + 1]])) m <- m + 1
      if (m == l) {
        searching <- Bool.false
      } else {
        g <- (d[[l + 1]] - d[[l]]) / (2 * e[[l]])
        r <- Num.sqrt(g * g + 1)
        g <- d[[m]] - d[[l]] + e[[l]] / (g + (if (g >= 0) r else 0 - r))
        s <- 1f64
        c <- 1f64
        p <- 0f64
        i <- m - 1
        underflow <- Bool.false
        while (i >= l && !underflow) {
          f <- s * e[[i]]
          b <- c * e[[i]]
          r <- Num.sqrt(f * f + g * g)
          e[[i + 1]] <- r
          if (r == 0) {
            d[[i + 1]] <- d[[i + 1]] - p
            e[[m]] <- 0f64
            underflow <- Bool.true
          } else {
            s <- f / r
            c <- g / r
            g <- d[[i + 1]] - p
            r <- (d[[i]] - g) * s + 2 * c * b
            p <- s * r
            d[[i + 1]] <- g + p
            g <- c * r - b
            i <- i - 1
          }
        }
        if (!underflow) {
          d[[l]] <- d[[l]] - p
          e[[l]] <- g
          e[[m]] <- 0f64
        }
      }
    }
  }
  d
}

# ---------------------------------------------------------------------------
# Driver: the same as spectralnorm_math.R's, except that the typed program returns the norm and
# the driver prints it inside the clock.
local({
  started <- proc.time()[[3]]
  output <- capture.output({
    options(digits = 10)
    cat(Num.to_r(execute(1000)), "\n")
  })
  seconds <- proc.time()[[3]] - started
  value <- NULL
  text <- c(output, capture.output(str(value, vec.len = 3, digits.d = 10, give.attr = FALSE)))
  bytes <- utf8ToInt(paste(text, collapse = "\n"))
  checksum <- sprintf("%d:%.0f", length(bytes), sum(as.numeric(bytes) * (seq_along(bytes) %% 251 + 1)))
  cat(sprintf("spectralnorm_math %s %.4f\n", checksum, seconds))
})
