# GNU R's side of the standard library benchmark: for each case in `ry.R`, the fastest idiomatic
# ways to do the same work in base R, purrr, stringr or stringi. A case lists several alternatives
# and reports the fastest, so its label names the R equivalent that set the bar. An alternative
# labeled `pkg::fun` is skipped when `pkg` is not installed.
#
# R holds n scalars as an atomic vector, so that is the input wherever an R user would have one.
# A function that takes a callback is compared against R's own callback idioms (`vapply`,
# `Filter`, `Reduce`, `Position`, a `for` loop, purrr), never against a vectorized rewrite of the
# callback's body. A function on one string is compared against R's vectorized string functions,
# because those are what an R user reaches for. R has no Result type, so a result is the tagged
# list `list(ok = value)` or `list(err = error)`, and a dictionary is a hashed environment or a
# named vector.
source(commandArgs(trailingOnly = TRUE)[[1]])

ok <- function(value) list(ok = value)
err <- function(error) list(err = error)
is_ok <- function(result) names(result) == "ok"

# List

case("List.from_vector", setup = { x <- doubles(n) }, as.list = as.list(x))
case("List.to_vector", setup = { l <- as.list(doubles(n)) }, unlist = unlist(l, use.names = FALSE),
  `vapply identity` = vapply(l, function(v) v, numeric(1)))
case("List.len", setup = { x <- doubles(n) }, length = {
  total <- 0
  for (i in seq_len(n)) total <- total + length(x)
  total
})
case("List.is_empty", setup = { x <- doubles(n) }, `length == 0` = {
  total <- 0L
  for (i in seq_len(n)) total <- total + (length(x) == 0L)
  total
})
case("List.get", setup = { x <- doubles(n); out <- vector("list", n) }, `[[` = {
  for (i in seq_len(n)) out[[i]] <- x[[i]]
  out
})
case("List.set", setup = { x <- doubles(n) }, `[<-` = {
  for (i in seq_len(n)) x[i] <- 0
  x
})
case("List.update", setup = { x <- doubles(n); f <- function(v) v + 1 }, `[<- f([])` = {
  for (i in seq_len(n)) x[i] <- f(x[i])
  x
})
case("List.append", `vector [length + 1] <-` = {
  y <- numeric()
  for (i in seq_len(n)) y[length(y) + 1L] <- i
  y
}, `list [[length + 1]] <-` = {
  y <- list()
  for (i in seq_len(n)) y[[length(y) + 1L]] <- i
  y
})
case("List.prepend", setup = { x <- doubles(n) }, c = {
  for (k in 1:10) y <- c(0, x)
  y
})
case("List.concat", setup = { x <- doubles(n) }, c = {
  for (k in 1:10) y <- c(x, x)
  y
})
case("List.single", setup = { out <- vector("list", n) }, list = {
  for (i in seq_len(n)) out[[i]] <- list(i)
  out
})
case("List.repeat", rep = rep(0, n), `rep list` = rep(list(0), n))
case("List.range", seq_len = seq_len(n), `:` = 1:n)
case("List.reverse", setup = { x <- doubles(n) }, rev = {
  for (k in 1:10) y <- rev(x)
  y
})
case("List.join", setup = { lists <- split(doubles(n), (seq_len(n) - 1L) %/% 1000L) }, unlist = {
  unlist(lists, use.names = FALSE)
}, `do.call c` = do.call(c, unname(lists)))
case("List.map", setup = { x <- doubles(n) }, vapply = vapply(x, function(v) v * 2, numeric(1)),
  lapply = lapply(x, function(v) v * 2), `purrr::map_dbl` = purrr::map_dbl(x, function(v) v * 2))
case("List.map_with_index", setup = { x <- doubles(n) }, vapply = {
  vapply(seq_along(x), function(i) x[[i]] * i, numeric(1))
}, mapply = mapply(function(v, i) v * i, x, seq_along(x)),
  `purrr::imap_dbl` = purrr::imap_dbl(x, function(v, i) v * i))
case("List.map2", setup = { x <- doubles(n) }, mapply = mapply(function(a, b) a + b, x, x),
  `purrr::map2_dbl` = purrr::map2_dbl(x, x, function(a, b) a + b))
case("List.map3", setup = { x <- doubles(n) }, mapply = mapply(function(a, b, c) a + b + c, x, x, x),
  `purrr::pmap_dbl` = purrr::pmap_dbl(list(x, x, x), function(a, b, c) a + b + c))
case("List.map4", setup = { x <- doubles(n) }, mapply = mapply(function(a, b, c, d) a + b + c + d, x, x, x, x),
  `purrr::pmap_dbl` = purrr::pmap_dbl(list(x, x, x, x), function(a, b, c, d) a + b + c + d))
case("List.join_map", setup = { x <- doubles(n) }, `unlist lapply` = {
  unlist(lapply(x, function(v) c(v, v)), use.names = FALSE)
}, `purrr::list_c` = purrr::list_c(purrr::map(x, function(v) c(v, v))))
case("List.keep_if", setup = { x <- doubles(n) }, Filter = Filter(function(v) v %% 2 == 0, x),
  `[vapply]` = x[vapply(x, function(v) v %% 2 == 0, logical(1))],
  `purrr::keep` = purrr::keep(x, function(v) v %% 2 == 0))
case("List.keep_if_try", setup = { x <- doubles(n) }, `[vapply]` = {
  checks <- lapply(x, function(v) ok(v %% 2 == 0))
  if (all(vapply(checks, is_ok, logical(1)))) ok(x[vapply(checks, function(r) r$ok, logical(1))]) else checks[[1]]
})
case("List.drop_if", setup = { x <- doubles(n) }, Filter = Filter(function(v) !(v %% 2 == 0), x),
  `[!vapply]` = x[!vapply(x, function(v) v %% 2 == 0, logical(1))],
  `purrr::discard` = purrr::discard(x, function(v) v %% 2 == 0))
case("List.keep_oks", setup = {
  x <- doubles(n)
  f <- function(v) if (v %% 2 == 0) ok(v) else err("Odd")
}, `unlist lapply` = unlist(lapply(lapply(x, f), function(r) r$ok)))
case("List.keep_errs", setup = {
  x <- doubles(n)
  f <- function(v) if (v %% 2 == 0) ok(v) else err(v)
}, `unlist lapply` = unlist(lapply(lapply(x, f), function(r) r$err)))
case("List.map_try", setup = {
  x <- doubles(n)
  f <- function(v) if (v > 0) ok(v * 2) else err("NotPositive")
}, `for loop` = {
  out <- numeric(n)
  failure <- NULL
  for (i in seq_len(n)) {
    r <- f(x[[i]])
    if (!is_ok(r)) {
      failure <- r
      break
    }
    out[[i]] <- r$ok
  }
  if (is.null(failure)) ok(out) else failure
})
case("List.walk", setup = { x <- doubles(n) }, `for loop` = {
  s <- 0
  for (v in x) s <- s + v
  s
}, Reduce = Reduce(function(s, v) s + v, x, 0), `purrr::reduce` = purrr::reduce(x, function(s, v) s + v, .init = 0))
case("List.walk_backwards", setup = { x <- doubles(n) }, `for loop` = {
  s <- 0
  for (i in rev(seq_along(x))) s <- s + x[[i]]
  s
}, `Reduce right` = Reduce(function(v, s) s + v, x, 0, right = TRUE))
case("List.walk_until", setup = { x <- doubles(n); stop_at <- n / 2 }, `for loop` = {
  s <- 0
  for (v in x) {
    if (v > stop_at) break
    s <- s + v
  }
  s
})
case("List.walk_backwards_until", setup = { x <- doubles(n); stop_at <- n / 2 }, `for loop` = {
  s <- 0
  for (i in rev(seq_along(x))) {
    if (x[[i]] < stop_at) break
    s <- s + x[[i]]
  }
  s
})
case("List.walk_with_index", setup = { x <- doubles(n) }, `for loop` = {
  s <- 0
  for (i in seq_along(x)) s <- s + x[[i]] * i
  s
})
case("List.walk_with_index_until", setup = { x <- doubles(n); stop_at <- n %/% 2L }, `for loop` = {
  s <- 0
  for (i in seq_along(x)) {
    if (i > stop_at) break
    s <- s + x[[i]] * i
  }
  s
})
case("List.walk_from", setup = { x <- doubles(n); from <- n %/% 2L }, `for loop` = {
  s <- 0
  for (i in from:length(x)) s <- s + x[[i]]
  s
})
case("List.walk_from_until", setup = { x <- doubles(n); from <- n %/% 4L; stop_at <- n / 2 }, `for loop` = {
  s <- 0
  for (i in from:length(x)) {
    if (x[[i]] > stop_at) break
    s <- s + x[[i]]
  }
  s
})
case("List.walk_try", setup = {
  x <- doubles(n)
  f <- function(s, v) if (v > 0) ok(s + v) else err("NotPositive")
}, `for loop` = {
  state <- ok(0)
  for (v in x) {
    state <- f(state$ok, v)
    if (!is_ok(state)) break
  }
  state
})
case("List.for_each", setup = { x <- doubles(n) }, `for loop` = {
  total <- 0
  for (v in x) total <- total + v
  total
}, `invisible lapply` = {
  total <- 0
  invisible(lapply(x, function(v) total <<- total + v))
  total
}, `purrr::walk` = {
  total <- 0
  purrr::walk(x, function(v) total <<- total + v)
  total
})
case("List.for_each_try", setup = {
  x <- doubles(n)
  total <- 0
  f <- function(v) if (v > 0) ok(total <<- total + v) else err("NotPositive")
}, `for loop` = {
  for (v in x) if (!is_ok(f(v))) break
  total
})
case("List.any", setup = { x <- doubles(n) }, `any vapply` = any(vapply(x, function(v) v < 0, logical(1))),
  Position = !is.na(Position(function(v) v < 0, x)), `purrr::some` = purrr::some(x, function(v) v < 0))
case("List.all", setup = { x <- doubles(n) }, `all vapply` = all(vapply(x, function(v) v > 0, logical(1))),
  `purrr::every` = purrr::every(x, function(v) v > 0))
case("List.contains", setup = { x <- doubles(n) }, `%in%` = {
  found <- 0L
  for (k in 1:10) found <- found + (-1 %in% x)
  found
}, `any ==` = {
  found <- 0L
  for (k in 1:10) found <- found + any(x == -1)
  found
})
case("List.count_if", setup = { x <- doubles(n) }, `sum vapply` = sum(vapply(x, function(v) v %% 3 == 0, logical(1))),
  `purrr::map_lgl` = sum(purrr::map_lgl(x, function(v) v %% 3 == 0)))
case("List.find_first", setup = { x <- doubles(n); target <- n / 2 }, Find = Find(function(v) v > target, x),
  `purrr::detect` = purrr::detect(x, function(v) v > target))
case("List.find_last", setup = { x <- doubles(n); target <- n / 2 }, `Find right` = {
  Find(function(v) v < target, x, right = TRUE)
}, `purrr::detect` = purrr::detect(x, function(v) v < target, .dir = "backward"))
case("List.find_first_index", setup = { x <- doubles(n); target <- n / 2 }, Position = Position(function(v) v > target, x),
  `purrr::detect_index` = purrr::detect_index(x, function(v) v > target))
case("List.find_last_index", setup = { x <- doubles(n); target <- n / 2 }, `Position right` = {
  Position(function(v) v < target, x, right = TRUE)
}, `purrr::detect_index` = purrr::detect_index(x, function(v) v < target, .dir = "backward"))
case("List.first", setup = { x <- doubles(n); out <- vector("list", n) }, `[[1]]` = {
  for (i in seq_len(n)) out[[i]] <- x[[1L]]
  out
})
case("List.last", setup = { x <- doubles(n); out <- vector("list", n) }, `[[length]]` = {
  for (i in seq_len(n)) out[[i]] <- x[[length(x)]]
  out
})
case("List.take_first", setup = { x <- doubles(n) }, head = {
  for (k in 1:10) y <- head(x, n %/% 2L)
  y
}, `[seq_len]` = {
  for (k in 1:10) y <- x[seq_len(n %/% 2L)]
  y
})
case("List.take_last", setup = { x <- doubles(n) }, tail = {
  for (k in 1:10) y <- tail(x, n %/% 2L)
  y
}, `[-seq_len]` = {
  for (k in 1:10) y <- x[-seq_len(n - n %/% 2L)]
  y
})
case("List.drop_first", setup = { x <- doubles(n) }, `[-seq_len]` = {
  for (k in 1:10) y <- x[-seq_len(n %/% 2L)]
  y
}, `tail -k` = {
  for (k in 1:10) y <- tail(x, -(n %/% 2L))
  y
})
case("List.drop_last", setup = { x <- doubles(n) }, `head -k` = {
  for (k in 1:10) y <- head(x, -(n %/% 2L))
  y
}, `[seq_len]` = {
  for (k in 1:10) y <- x[seq_len(n - n %/% 2L)]
  y
})
case("List.drop_at", setup = { x <- doubles(n) }, `[-i]` = {
  for (k in 1:10) y <- x[-(n %/% 2L)]
  y
})
case("List.sublist", setup = { x <- doubles(n) }, `[a:b]` = {
  for (k in 1:10) y <- x[(n %/% 4L):(n %/% 4L + n %/% 2L - 1L)]
  y
})
case("List.split_at", setup = { x <- doubles(n) }, `list(head, tail)` = {
  for (k in 1:10) parts <- list(before = x[seq_len(n %/% 2L)], others = x[-seq_len(n %/% 2L)])
  parts
})
case("List.split_first", setup = { x <- doubles(n); delimiter <- n / 2 }, match = {
  for (k in 1:10) {
    at <- match(delimiter, x)
    parts <- if (is.na(at)) err("NotFound") else ok(list(before = x[seq_len(at - 1L)], after = x[-seq_len(at)]))
  }
  parts
})
case("List.split_last", setup = { x <- doubles(n); delimiter <- n / 2 }, `max which` = {
  for (k in 1:10) {
    hits <- which(x == delimiter)
    at <- if (length(hits)) max(hits) else NA
    parts <- if (is.na(at)) err("NotFound") else ok(list(before = x[seq_len(at - 1L)], after = x[-seq_len(at)]))
  }
  parts
})
case("List.split_on", setup = { x <- doubles(n) %% 10 }, `split cumsum` = {
  is_delimiter <- x == 0
  runs <- cumsum(is_delimiter)
  split(x[!is_delimiter], factor(runs[!is_delimiter], levels = 0:max(runs)))
})
case("List.split_on_list", setup = { x <- doubles(n) %% 10 }, `split cumsum` = {
  starts <- which(x[-length(x)] == 9 & x[-1L] == 0)
  is_delimiter <- logical(length(x))
  is_delimiter[c(starts, starts + 1L)] <- TRUE
  runs <- cumsum(seq_along(x) %in% starts)
  split(x[!is_delimiter], factor(runs[!is_delimiter], levels = 0:max(runs)))
})
case("List.chunks_of", setup = { x <- doubles(n) }, split = unname(split(x, (seq_along(x) - 1L) %/% 10L)))
case("List.intersperse", setup = { x <- doubles(n) }, `rbind` = as.vector(rbind(x, 0))[-(2L * length(x))])
case("List.sort_with", setup = { x <- scrambled(n) }, `sort (no comparator in R)` = sort(x), `[order]` = x[order(x)])
case("List.sort_asc", setup = { x <- scrambled(n) }, sort = sort(x), `sort radix` = sort(x, method = "radix"))
case("List.sort_desc", setup = { x <- scrambled(n) }, `sort decreasing` = sort(x, decreasing = TRUE),
  `sort radix decreasing` = sort(x, decreasing = TRUE, method = "radix"))
case("List.swap", setup = { x <- doubles(n) }, `[<- pair` = {
  for (i in seq_len(n %/% 2L)) {
    j <- n - i + 1L
    held <- x[[i]]
    x[[i]] <- x[[j]]
    x[[j]] <- held
  }
  x
})
case("List.replace", setup = { x <- doubles(n) }, `[[ then [[<-` = {
  total <- 0
  for (i in seq_len(n)) {
    total <- total + x[[i]]
    x[[i]] <- 0
  }
  total
})
case("List.append_if_ok", setup = {
  results <- lapply(doubles(n), function(v) if (v %% 2 == 0) ok(v) else err("Odd"))
}, `[length + 1] <-` = {
  y <- numeric()
  for (i in seq_len(n)) {
    r <- results[[i]]
    if (is_ok(r)) y[length(y) + 1L] <- r$ok
  }
  y
})
case("List.prepend_if_ok", setup = { x <- doubles(n); r <- ok(0) }, c = {
  for (k in 1:10) y <- if (is_ok(r)) c(r$ok, x) else x
  y
})
case("List.concat_utf8", setup = { bytes <- seq_len(n) %% 128L }, `c charToRaw` = {
  for (k in 1:10) y <- c(bytes, as.integer(charToRaw("abc")))
  y
})
case("List.with_capacity", `preallocated [[<-` = {
  y <- numeric(n)
  for (i in seq_len(n)) y[[i]] <- i
  y
})
case("List.reserve", `preallocated [[<-` = {
  y <- numeric(n)
  for (i in seq_len(n)) y[[i]] <- i
  y
})
case("List.release_excess_capacity")
case("List.sum", setup = { x <- doubles(n) }, sum = {
  total <- 0
  for (k in 1:10) total <- total + sum(x)
  total
})
case("List.product", setup = { x <- 1 + doubles(n) * 1e-12 }, prod = {
  total <- 0
  for (k in 1:10) total <- total + prod(x)
  total
})
case("List.min", setup = { x <- scrambled(n) }, min = {
  total <- 0
  for (k in 1:10) total <- total + min(x)
  total
})
case("List.max", setup = { x <- scrambled(n) }, max = {
  total <- 0
  for (k in 1:10) total <- total + max(x)
  total
})
case("List.starts_with", setup = { x <- doubles(n); prefix <- doubles(n %/% 2L) }, `identical head` = {
  found <- 0L
  for (k in 1:10) found <- found + identical(x[seq_along(prefix)], prefix)
  found
}, `all ==` = {
  found <- 0L
  for (k in 1:10) found <- found + all(x[seq_along(prefix)] == prefix)
  found
})
case("List.ends_with", setup = { x <- doubles(n); suffix <- doubles(n)[-seq_len(n %/% 2L)] }, `identical tail` = {
  found <- 0L
  for (k in 1:10) found <- found + identical(x[(length(x) - length(suffix) + 1L):length(x)], suffix)
  found
}, `all ==` = {
  found <- 0L
  for (k in 1:10) found <- found + all(x[(length(x) - length(suffix) + 1L):length(x)] == suffix)
  found
})

# Str

case("Str.is_empty", setup = { s <- words(n) }, `!nzchar` = !nzchar(s), `== ""` = s == "",
  `stringi::stri_isempty` = stringi::stri_isempty(s))
case("Str.concat", setup = { s <- words(n) }, paste0 = paste0(s, s), `stringi::stri_c` = stringi::stri_c(s, s),
  `stringr::str_c` = stringr::str_c(s, s))
case("Str.join_with", setup = { s <- words(n) }, `paste collapse` = paste(s, collapse = ","),
  `stringi::stri_flatten` = stringi::stri_flatten(s, collapse = ","),
  `stringr::str_flatten` = stringr::str_flatten(s, collapse = ","))
case("Str.split_on", setup = { joined <- paste(words(n), collapse = ",") }, `strsplit fixed` = {
  strsplit(joined, ",", fixed = TRUE)[[1L]]
}, `stringi::stri_split_fixed` = stringi::stri_split_fixed(joined, ",")[[1L]],
  `stringr::str_split_1` = stringr::str_split_1(joined, stringr::fixed(",")))
case("Str.repeat", setup = { s <- words(n) }, strrep = strrep(s, 3L), `stringi::stri_dup` = stringi::stri_dup(s, 3L),
  `stringr::str_dup` = stringr::str_dup(s, 3L))
case("Str.starts_with", setup = { s <- words(n) }, startsWith = startsWith(s, "item_00"),
  `stringi::stri_startswith_fixed` = stringi::stri_startswith_fixed(s, "item_00"),
  `stringr::str_starts` = stringr::str_starts(s, stringr::fixed("item_00")))
case("Str.ends_with", setup = { s <- words(n) }, endsWith = endsWith(s, "5_value"),
  `stringi::stri_endswith_fixed` = stringi::stri_endswith_fixed(s, "5_value"),
  `stringr::str_ends` = stringr::str_ends(s, stringr::fixed("5_value")))
case("Str.contains", setup = { s <- words(n) }, `grepl fixed` = grepl("55", s, fixed = TRUE),
  `stringi::stri_detect_fixed` = stringi::stri_detect_fixed(s, "55"),
  `stringr::str_detect` = stringr::str_detect(s, stringr::fixed("55")))
case("Str.trim", setup = { s <- padded(n) }, trimws = trimws(s), `stringi::stri_trim_both` = stringi::stri_trim_both(s),
  `stringr::str_trim` = stringr::str_trim(s))
case("Str.trim_start", setup = { s <- padded(n) }, `trimws left` = trimws(s, which = "left"),
  `stringi::stri_trim_left` = stringi::stri_trim_left(s), `stringr::str_trim` = stringr::str_trim(s, side = "left"))
case("Str.trim_end", setup = { s <- padded(n) }, `trimws right` = trimws(s, which = "right"),
  `stringi::stri_trim_right` = stringi::stri_trim_right(s), `stringr::str_trim` = stringr::str_trim(s, side = "right"))
case("Str.drop_prefix", setup = { s <- words(n) }, `sub ^` = sub("^item_", "", s),
  `startsWith substring` = { hit <- startsWith(s, "item_"); s[hit] <- substring(s[hit], 6L); s },
  `stringi::stri_replace_first_regex` = stringi::stri_replace_first_regex(s, "^item_", ""),
  `stringr::str_remove` = stringr::str_remove(s, "^item_"))
case("Str.drop_suffix", setup = { s <- words(n) }, `sub $` = sub("_value$", "", s),
  `endsWith substr` = { hit <- endsWith(s, "_value"); s[hit] <- substr(s[hit], 1L, nchar(s[hit]) - 6L); s },
  `stringi::stri_replace_last_regex` = stringi::stri_replace_last_regex(s, "_value$", ""),
  `stringr::str_remove` = stringr::str_remove(s, "_value$"))
case("Str.with_prefix", setup = { s <- words(n) }, paste0 = paste0("my_", s), `stringi::stri_c` = stringi::stri_c("my_", s))
case("Str.replace_each", setup = { s <- words(n) }, `gsub fixed` = gsub("_", "-", s, fixed = TRUE),
  `stringi::stri_replace_all_fixed` = stringi::stri_replace_all_fixed(s, "_", "-"),
  `stringr::str_replace_all` = stringr::str_replace_all(s, stringr::fixed("_"), "-"))
case("Str.replace_first", setup = { s <- words(n) }, `sub fixed` = sub("_", "-", s, fixed = TRUE),
  `stringi::stri_replace_first_fixed` = stringi::stri_replace_first_fixed(s, "_", "-"),
  `stringr::str_replace` = stringr::str_replace(s, stringr::fixed("_"), "-"))
case("Str.replace_last", setup = { s <- words(n) }, `sub greedy` = sub("^(.*)_", "\\1-", s),
  `stringi::stri_replace_last_fixed` = stringi::stri_replace_last_fixed(s, "_", "-"))
case("Str.split_first", setup = { s <- words(n) }, `regexpr substr` = {
  at <- regexpr("_", s, fixed = TRUE)
  list(before = substr(s, 1L, at - 1L), after = substring(s, at + 1L))
}, `stringi::stri_split_fixed` = stringi::stri_split_fixed(s, "_", n = 2L))
case("Str.split_last", setup = { s <- words(n) }, `sub greedy` = {
  list(before = sub("_[^_]*$", "", s), after = sub("^.*_", "", s))
}, `stringi::stri_locate_last_fixed` = {
  at <- stringi::stri_locate_last_fixed(s, "_")[, 1L]
  list(before = stringi::stri_sub(s, 1L, at - 1L), after = stringi::stri_sub(s, at + 1L))
})
case("Str.count_utf8_bytes", setup = { s <- words(n) }, `nchar bytes` = nchar(s, type = "bytes"),
  `stringi::stri_numbytes` = stringi::stri_numbytes(s))
case("Str.to_utf8", setup = { s <- words(n) }, `lapply charToRaw` = lapply(s, function(x) as.integer(charToRaw(x))),
  `lapply utf8ToInt (ASCII)` = lapply(s, utf8ToInt))
case("Str.from_utf8", setup = { bytes <- lapply(words(n), utf8ToInt) }, `vapply intToUtf8` = vapply(bytes, intToUtf8, ""),
  `stringi::stri_enc_fromutf32 (ASCII)` = stringi::stri_enc_fromutf32(bytes))
case("Str.from_utf8_lossy", setup = { bytes <- lapply(words(n), utf8ToInt) }, `vapply intToUtf8` = vapply(bytes, intToUtf8, ""),
  `stringi::stri_enc_fromutf32 (ASCII)` = stringi::stri_enc_fromutf32(bytes))
case("Str.to_integer", setup = { s <- as.character(seq_len(n)) }, as.integer = as.integer(s),
  strtoi = strtoi(s, 10L))
case("Str.to_double", setup = { s <- as.character(seq_len(n) / 4) }, as.double = as.double(s))
case("Str.with_ascii_lowercased", setup = { s <- toupper(words(n)) }, tolower = tolower(s),
  `stringi::stri_trans_tolower` = stringi::stri_trans_tolower(s), `stringr::str_to_lower` = stringr::str_to_lower(s))
case("Str.with_ascii_uppercased", setup = { s <- words(n) }, toupper = toupper(s),
  `stringi::stri_trans_toupper` = stringi::stri_trans_toupper(s), `stringr::str_to_upper` = stringr::str_to_upper(s))
case("Str.caseless_ascii_equals", setup = { s <- words(n); upper <- toupper(words(n)) }, `tolower ==` = {
  tolower(s) == tolower(upper)
}, `stringi::stri_trans_tolower ==` = stringi::stri_trans_tolower(s) == stringi::stri_trans_tolower(upper))
case("Str.walk_utf8", setup = { text <- strrep("abcdefghij", n %/% 10L) }, `for loop charToRaw` = {
  s <- 0L
  for (byte in as.integer(charToRaw(text))) s <- s + byte
  s
}, `Reduce charToRaw` = Reduce(function(s, byte) s + byte, as.integer(charToRaw(text)), 0L))
case("Str.walk_utf8_with_index", setup = { text <- strrep("abcdefghij", n %/% 10L) }, `for loop charToRaw` = {
  bytes <- as.integer(charToRaw(text))
  s <- 0
  for (i in seq_along(bytes)) s <- s + bytes[[i]] * i
  s
})
case("Str.to_utf16", setup = { s <- words(n) }, `lapply utf8ToInt (BMP)` = lapply(s, utf8ToInt))
case("Str.from_utf16", setup = { units <- lapply(words(n), utf8ToInt) }, `vapply intToUtf8 (BMP)` = vapply(units, intToUtf8, ""))
case("Str.from_utf16_lossy", setup = { units <- lapply(words(n), utf8ToInt) }, `vapply intToUtf8 (BMP)` = {
  vapply(units, intToUtf8, "")
})
case("Str.to_utf32", setup = { s <- words(n) }, `lapply utf8ToInt` = lapply(s, utf8ToInt),
  `stringi::stri_enc_toutf32` = stringi::stri_enc_toutf32(s))
case("Str.from_utf32", setup = { points <- lapply(words(n), utf8ToInt) }, `vapply intToUtf8` = vapply(points, intToUtf8, ""),
  `stringi::stri_enc_fromutf32` = stringi::stri_enc_fromutf32(points))
case("Str.from_utf32_lossy", setup = { points <- lapply(words(n), utf8ToInt) }, `vapply intToUtf8` = {
  vapply(points, intToUtf8, "")
}, `stringi::stri_enc_fromutf32` = stringi::stri_enc_fromutf32(points))

# Dict. `filled` builds a hashed environment from `k1` to `kn`, each key's value its number, and
# `named` the same entries as a named vector, which keeps insertion order.

filled <- function(count) list2env(setNames(as.list(doubles(count)), keys(count)), hash = TRUE)
named <- function(count) setNames(doubles(count), keys(count))

case("Dict.empty", new.env = {
  for (i in seq_len(n)) d <- new.env(hash = TRUE)
  d
})
case("Dict.with_capacity", setup = { k <- keys(n) }, `new.env size` = {
  d <- new.env(hash = TRUE, size = n)
  for (i in seq_len(n)) d[[k[[i]]]] <- i
  d
})
case("Dict.single", setup = { out <- vector("list", n) }, list2env = {
  for (i in seq_len(n)) out[[i]] <- list2env(list(key = i))
  out
}, `named vector` = {
  for (i in seq_len(n)) out[[i]] <- c(key = i)
  out
})
case("Dict.from_list", setup = { k <- keys(n); v <- doubles(n) }, list2env = list2env(setNames(as.list(v), k), hash = TRUE),
  setNames = setNames(v, k))
case("Dict.to_list", setup = { d <- filled(n) }, `as.list + names` = {
  entries <- as.list(d)
  list(names(entries), unlist(entries, use.names = FALSE))
})
case("Dict.len", setup = { d <- filled(1000L) }, length = {
  total <- 0
  for (i in seq_len(n)) total <- total + length(d)
  total
})
case("Dict.is_empty", setup = { d <- filled(1000L) }, `length == 0` = {
  total <- 0L
  for (i in seq_len(n)) total <- total + (length(d) == 0L)
  total
})
case("Dict.get", setup = { d <- filled(n); v <- named(n); k <- rev(keys(n)); out <- vector("list", n) }, `for loop [[` = {
  for (i in seq_len(n)) out[[i]] <- d[[k[[i]]]]
  out
}, mget = mget(k, envir = d), `named vector [keys]` = v[k])
case("Dict.contains", setup = { d <- filled(n); v <- named(n); k <- rev(keys(n)) }, `for loop exists` = {
  found <- 0L
  for (i in seq_len(n)) found <- found + exists(k[[i]], envir = d, inherits = FALSE)
  found
}, `%in% names` = sum(k %in% names(v)))
case("Dict.insert", setup = { k <- keys(n) }, `env [[<-` = {
  d <- new.env(hash = TRUE)
  for (i in seq_len(n)) d[[k[[i]]]] <- i
  d
}, assign = {
  d <- new.env(hash = TRUE)
  for (i in seq_len(n)) assign(k[[i]], i, envir = d)
  d
})
case("Dict.remove", setup = { d <- filled(n); k <- keys(n) }, `for loop rm` = {
  for (i in seq_len(n %/% 2L)) rm(list = k[[i]], envir = d)
  d
}, `rm list` = {
  rm(list = k[seq_len(n %/% 2L)], envir = d)
  d
})
case("Dict.update", setup = { w <- keys(1000L)[seq_len(n) %% 1000L + 1L] }, `env [[<-` = {
  counts <- new.env(hash = TRUE)
  for (i in seq_len(n)) {
    key <- w[[i]]
    found <- counts[[key]]
    counts[[key]] <- (if (is.null(found)) 0L else found) + 1L
  }
  counts
}, table = table(w), `tabulate match` = tabulate(match(w, unique(w))))
case("Dict.keys", setup = { d <- filled(n) }, names = names(d), `ls unsorted` = ls(d, sorted = FALSE))
case("Dict.values", setup = { d <- filled(n) }, as.list = as.list(d), `mget names` = mget(names(d), envir = d),
  eapply = eapply(d, identity))
case("Dict.walk", setup = { d <- filled(n) }, `for loop names` = {
  s <- 0
  for (key in names(d)) s <- s + d[[key]]
  s
}, `Reduce as.list` = Reduce(function(s, v) s + v, as.list(d), 0))
case("Dict.walk_until", setup = { v <- named(n); stop_at <- n / 2 }, `for loop named vector` = {
  s <- 0
  for (i in seq_along(v)) {
    if (v[[i]] > stop_at) break
    s <- s + v[[i]]
  }
  s
})
case("Dict.map", setup = { d <- filled(n) }, `list2env lapply` = list2env(lapply(as.list(d), function(v) v * 2), hash = TRUE),
  eapply = eapply(d, function(v) v * 2))
case("Dict.join_map", setup = { d <- filled(n) }, `for loop env` = {
  out <- new.env(hash = TRUE)
  for (key in names(d)) out[[key]] <- d[[key]] + 1
  out
})
case("Dict.keep_if", setup = { d <- filled(n) }, `list2env Filter` = {
  list2env(Filter(function(v) v %% 2 == 0, as.list(d)), hash = TRUE)
}, `[vapply]` = {
  entries <- as.list(d)
  entries[vapply(entries, function(v) v %% 2 == 0, logical(1))]
})
case("Dict.drop_if", setup = { d <- filled(n) }, `list2env Filter` = {
  list2env(Filter(function(v) !(v %% 2 == 0), as.list(d)), hash = TRUE)
}, `[!vapply]` = {
  entries <- as.list(d)
  entries[!vapply(entries, function(v) v %% 2 == 0, logical(1))]
})
case("Dict.insert_all", setup = { first <- filled(n); second <- filled(n %/% 2L); a <- named(n); b <- named(n %/% 2L) }, list2env = {
  list2env(as.list(second), envir = list2env(as.list(first), hash = TRUE))
}, `named vector [<-` = {
  a[names(b)] <- b
  a
}, modifyList = utils::modifyList(as.list(a), as.list(b)))
case("Dict.keep_shared", setup = { a <- named(n); b <- named(n %/% 2L) }, `named vector %in%` = {
  shared <- names(a)[names(a) %in% names(b)]
  a[shared][a[shared] == b[shared]]
})
case("Dict.remove_all", setup = { a <- named(n); b <- named(n %/% 2L) }, `named vector !%in%` = a[!(names(a) %in% names(b))])
case("Dict.reserve", setup = { k <- keys(n) }, `new.env size` = {
  d <- new.env(hash = TRUE, size = n)
  for (i in seq_len(n)) d[[k[[i]]]] <- i
  d
})
case("Dict.capacity")
case("Dict.release_excess_capacity")
case("Dict.clear", setup = { d <- filled(n) }, new.env = new.env(hash = TRUE), `rm names` = {
  rm(list = names(d), envir = d)
  d
})

# Result. Every case maps over n results, half `ok(x)` for an even x and half `err("Odd")`.

results <- function(count) lapply(doubles(count), function(v) if (v %% 2 == 0) ok(v) else err("Odd"))

case("Result.is_ok", setup = { rs <- results(n) }, vapply = vapply(rs, is_ok, logical(1)),
  `purrr::map_lgl` = purrr::map_lgl(rs, is_ok))
case("Result.is_err", setup = { rs <- results(n) }, vapply = vapply(rs, function(r) !is_ok(r), logical(1)))
case("Result.with_default", setup = { rs <- results(n) }, vapply = {
  vapply(rs, function(r) if (is_ok(r)) r$ok else 0, numeric(1))
})
case("Result.map", setup = { rs <- results(n) }, lapply = lapply(rs, function(r) if (is_ok(r)) ok(r$ok * 2) else r))
case("Result.map_err", setup = { rs <- results(n) }, lapply = lapply(rs, function(r) if (is_ok(r)) r else err("odd")))
case("Result.map_both", setup = { rs <- results(n) }, lapply = {
  lapply(rs, function(r) if (is_ok(r)) ok(r$ok * 2) else err("odd"))
})
case("Result.map2", setup = { rs <- results(n) }, mapply = {
  mapply(function(a, b) if (!is_ok(a)) a else if (!is_ok(b)) b else ok(a$ok + b$ok), rs, rs, SIMPLIFY = FALSE)
}, `purrr::map2` = purrr::map2(rs, rs, function(a, b) if (!is_ok(a)) a else if (!is_ok(b)) b else ok(a$ok + b$ok)))
case("Result.and_then", setup = { rs <- results(n); half <- n / 2 }, lapply = {
  lapply(rs, function(r) if (!is_ok(r)) r else if (r$ok > half) ok(r$ok) else err("Small"))
})
case("Result.on_err", setup = { rs <- results(n) }, lapply = lapply(rs, function(r) if (is_ok(r)) r else ok(0)))
