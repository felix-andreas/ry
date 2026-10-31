sum_squares <- function(n) { s <- 0; for (i in seq_len(n)) s <- s + i * i; s }
for (k in 1:3) print(sum_squares(100))
collatz <- function(n) { steps <- 0L; while (n != 1) { if (n %% 2 == 0) n <- n / 2 else n <- 3 * n + 1; steps <- steps + 1L }; steps }
vapply(c(6, 7, 27), collatz, integer(1))
fill <- function(n) { v <- numeric(n); for (i in 1:n) v[i] <- i / 2; v }
fill(5); fill(5)
shadow <- function() { `+` <- function(a, b) paste(a, b); 1 + 2 }
shadow(); shadow(); shadow()
breaky <- function() { out <- c(); for (i in 1:10) { if (i %% 3 == 0) next; if (i > 8) break; out <- c(out, i) }; out }
breaky(); breaky()
overflow <- function() { x <- .Machine$integer.max; x + 1L }
overflow(); overflow()
na_logic <- function(a) { if (is.na(a) || a) "na or true" else "false" }
na_logic(NA); na_logic(FALSE); na_logic(TRUE)
nested <- function() { total <- 0; for (i in 1:3) for (j in 1:3) { if (j == 2) next; total <- total + i * j }; total }
nested(); nested()
