# Big integers as a sign (-1, 0 or 1) and a magnitude. A magnitude is a list of base-10000
# limbs, least significant first, with no leading zero limb, so zero is the empty list.
# Division searches for the quotient between powers of ten, as the R program does.
alias Nat = List<I64>
alias Int = Record{sign: I64, magnitude: Nat}

execute <- function(digits: I64) -> List<Str> {
  one <- int(1)
  two <- int(2)
  three <- int(3)
  ten <- int(10)
  i: I64 <- 0
  k: I64 <- 0
  ns: I64 <- 0
  k1: I64 <- 1
  a <- int(0)
  n <- one
  d <- one
  output <- []
  while (Bool.true) {
    k <- k + 1
    t <- times(n, two)
    n <- times(n, int(k))
    a <- plus(a, t)
    k1 <- k1 + 2
    a <- times(a, int(k1))
    d <- times(d, int(k1))
    if (!int_less(a, n)) {
      n3a <- plus(times(n, three), a)
      t <- quotient(n3a, d)
      td <- times(t, d)
      u <- plus(minus(n3a, td), n)
      if (int_less(u, d)) {
        ns <- ns * 10 + t$sign * List.sum(t$magnitude)
        i <- i + 1
        if (i %% 5 == 0) {
          output <- List.append(output, zero_pad(ns, 5))
          if (i %% 2 == 0)
            output <- List.append(output, f"\t:{i}\n")
          ns <- 0i64
        }
        if (i >= digits)
          return(output)
        a <- minus(a, td)
        a <- times(a, ten)
        n <- times(n, ten)
      }
    }
  }
  output
}

int <- function(value: I64) -> Int
  signed(1, nat(value))

signed <- function(sign: I64, magnitude: Nat) -> Int
  (sign = if (List.is_empty(magnitude)) 0 else sign, magnitude = magnitude)

plus <- function(x: Int, y: Int) -> Int {
  if (x$sign == 0)
    return(y)
  if (y$sign == 0)
    return(x)
  if (x$sign == y$sign)
    return(signed(x$sign, add(x$magnitude, y$magnitude)))
  match (compare(x$magnitude, y$magnitude)) {
    :EQ => int(0)
    :GT => signed(x$sign, subtract(x$magnitude, y$magnitude))
    :LT => signed(y$sign, subtract(y$magnitude, x$magnitude))
  }
}

minus <- function(x: Int, y: Int) -> Int
  plus(x, (..y, sign = -y$sign))

times <- function(x: Int, y: Int) -> Int
  signed(x$sign * y$sign, multiply(x$magnitude, y$magnitude))

quotient <- function(x: Int, y: Int) -> Int
  signed(x$sign * y$sign, divide(x$magnitude, y$magnitude))

int_less <- function(x: Int, y: Int) -> Bool {
  if (x$sign != y$sign)
    return(x$sign < y$sign)
  if (x$sign > 0)
    return(less(x$magnitude, y$magnitude))
  if (x$sign < 0)
    return(less(y$magnitude, x$magnitude))
  Bool.false
}

nat <- function(value: I64) -> Nat {
  limbs <- []
  rest <- value
  while (rest > 0) {
    limbs <- List.append(limbs, rest %% 10000)
    rest <- rest %/% 10000
  }
  limbs
}

compare <- function(x: Nat, y: Nat) -> :LT | :EQ | :GT {
  if (List.len(x) != List.len(y))
    return(Num.compare(List.len(x), List.len(y)))
  i <- List.len(x)
  while (i >= 1) {
    if (x[[i]] != y[[i]])
      return(Num.compare(x[[i]], y[[i]]))
    i <- i - 1
  }
  :EQ
}

less <- function(x: Nat, y: Nat) -> Bool {
  match (compare(x, y)) {
    :LT => Bool.true
    _ => Bool.false
  }
}

add <- function(x: Nat, y: Nat) -> Nat {
  result <- []
  carry: I64 <- 0
  for (i in List.range(1, Num.max(List.len(x), List.len(y)))) {
    sum <- carry + limb(x, i) + limb(y, i)
    result <- List.append(result, sum %% 10000)
    carry <- sum %/% 10000
  }
  if (carry > 0) List.append(result, carry) else result
}

# `x - y` for `x >= y`.
subtract <- function(x: Nat, y: Nat) -> Nat {
  result <- []
  borrow: I64 <- 0
  for (i in List.range(1, List.len(x))) {
    difference <- x[[i]] - limb(y, i) - borrow
    if (difference < 0) {
      result <- List.append(result, difference + 10000)
      borrow <- 1i64
    } else {
      result <- List.append(result, difference)
      borrow <- 0i64
    }
  }
  trim(result)
}

multiply <- function(x: Nat, y: Nat) -> Nat {
  if (List.is_empty(x) || List.is_empty(y))
    return([])
  result <- List.repeat(0i64, List.len(x) + List.len(y))
  for (i in List.range(1, List.len(x))) {
    carry: I64 <- 0
    for (j in List.range(1, List.len(y))) {
      product <- result[[i + j - 1]] + x[[i]] * y[[j]] + carry
      result[[i + j - 1]] <- product %% 10000
      carry <- product %/% 10000
    }
    result[[i + List.len(y)]] <- carry
  }
  trim(result)
}

divide <- function(x: Nat, y: Nat) -> Nat {
  match (compare(x, y)) {
    :LT => []
    :EQ => nat(1)
    :GT => {
      if (List.len(y) == 1 && y[[1]] == 1)
        return(x)
      x_log10 <- log10(x)
      y_log10 <- log10(y)
      lo_log10 <- x_log10 - y_log10 - (if (x_log10 != y_log10) 1 else 0)
      hi_log10 <- x_log10 - y_log10 + 1
      lo <- pow10(lo_log10)
      hi <- pow10(hi_log10)
      if (lo_log10 == 0 && hi_log10 > 1) {
        lo10 <- multiply(lo, nat(10))
        if (less(x, multiply(lo10, y))) {
          hi <- lo10
        } else {
          lo <- lo10
        }
      }
      while (less(lo, hi)) {
        mid <- half(add(add(lo, hi), nat(1)))
        if (less(x, multiply(mid, y))) {
          hi <- subtract(mid, nat(1))
        } else {
          lo <- mid
        }
      }
      lo
    }
  }
}

half <- function(x: Nat) -> Nat {
  result <- x
  remainder: I64 <- 0
  i <- List.len(x)
  while (i >= 1) {
    value <- remainder * 10000 + x[[i]]
    result[[i]] <- value %/% 2
    remainder <- value %% 2
    i <- i - 1
  }
  trim(result)
}

# The number of decimal digits less one.
log10 <- function(x: Nat) -> I64 {
  top <- x[[List.len(x)]]
  count: I64 <- 0
  while (top >= 10) {
    top <- top %/% 10
    count <- count + 1
  }
  4 * (List.len(x) - 1) + count
}

pow10 <- function(exponent: I64) -> Nat
  List.append(List.repeat(0i64, exponent %/% 4), Num.pow_int(10, exponent %% 4))

limb <- function(x: Nat, i: I64) -> I64
  if (i <= List.len(x)) x[[i]] else 0

trim <- function(x: Nat) -> Nat {
  result <- x
  while (!List.is_empty(result) && result[[List.len(result)]] == 0)
    result <- List.drop_last(result, 1)
  result
}

zero_pad <- function(value: I64, width: I64) -> Str {
  text <- f"{value}"
  Str.concat(Str.repeat("0", Num.max(0, width - List.len(Str.to_utf8(text)))), text)
}

# ---------------------------------------------------------------------------
# Driver: the same as pidigits.R's, except that the typed program returns its output and
# the driver prints it inside the clock.
local({
  started <- proc.time()[[3]]
  output <- capture.output(cat(List.to_r(execute(55)), sep = ""))
  seconds <- proc.time()[[3]] - started
  value <- NULL
  text <- c(output, capture.output(str(value, vec.len = 3, digits.d = 10, give.attr = FALSE)))
  bytes <- utf8ToInt(paste(text, collapse = "\n"))
  checksum <- sprintf("%d:%.0f", length(bytes), sum(as.numeric(bytes) * (seq_along(bytes) %% 251 + 1)))
  cat(sprintf("pidigits %s %.4f\n", checksum, seconds))
})
