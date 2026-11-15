# Hot loops whose variables hold a compact sequence on some passes and something else on others:
# `1:line` that makes the scalar `1:1` before longer sequences, a sequence replaced by `which()`,
# a sequence assigned into in place, and a loop over a sequence or `integer(0)`.
chunks <- function(total, width) {
    out <- 0L
    while (total > 0L) {
        line <- min(width, total)
        inds <- 1:line
        lo <- rep.int(1L, line)
        hi <- rep.int(10L, line)
        repeat {
            lo[inds] <- lo[inds] + 1L
            inds <- which(lo < hi)
            if (!length(inds)) break
        }
        out <- out + sum(lo)
        total <- total - line
    }
    out
}
for (k in 1:40) r <- chunks(k, 3L)
r
for (k in 1:40) r <- chunks(100L, k)
r
rotate <- function(n) {
    perm1 <- 1:n
    flips <- 0L
    for (step in 1:20) {
        if (k <- perm1[[1L]]) flips <- flips + k
        perm <- perm1
        perm[1:2] <- perm[2:1]
        perm1[1:(n - 1L)] <- perm1[2:n]
        perm1[[n]] <- perm[[1L]]
    }
    c(flips, perm1)
}
for (k in 1:40) r <- rotate(5L)
r
r <- rotate(2L)
r
sweep <- function(n) {
    acc <- 0
    for (j in 1:30) {
        rx <- if (j %% 3L == 0L) integer(0) else 3:(n - 1L)
        for (r in rx) acc <- acc + r * j
        single <- j:j
        for (s in single) acc <- acc - s
    }
    acc
}
for (k in 1:40) r <- sweep(k + 3L)
r
sweep(4L)
# A compact sequence bound in an enclosing frame, read and then assigned into by a hot loop:
# the assignment makes a local copy and leaves the outer binding as it was.
outer <- function() {
    v <- 1:10
    inner <- function() {
        s <- 0L
        for (i in 1:3000) {
            s <- s + v[[3L]]
            if (i == 2000L) v[[3L]] <- 100L
        }
        c(s, v[[3L]])
    }
    c(inner(), v[[3L]])
}
outer()
# A sequence the loop swaps and increments element by element.
shuffle <- function(n) {
    p <- 1:n
    q <- seq_len(n) + 0
    for (j in 1:5000) {
        t <- p[[1L]]; p[[1L]] <- p[[2L]]; p[[2L]] <- t
        p[[3L]] <- p[[3L]] + 1L
        q[[n]] <- q[[n]] / 2 + j
    }
    list(p, q)
}
shuffle(5L)
# `lapply` and `vapply` read a compact sequence one element at a time.
squares <- lapply(seq_len(2000L), function(x) x * (x + 0.5))
sum(unlist(squares))
sum(vapply(3:2002, function(x) x / 2, numeric(1)))
lapply(seq(2, 8, by = 3), function(x) x)
