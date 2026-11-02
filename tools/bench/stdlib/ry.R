# ry's side of the standard library benchmark: one case per `List`, `Str`, `Dict` and `Result`
# function, named after it. `gnu.R` holds the GNU R equivalents under the same names, and
# `run.py` runs both and joins them. A function that works on one value is timed over n calls,
# usually through `List.map` over n inputs, which is how ry code applies it to a collection.
# Where the input is a vector, the case also times an `array` alternative over the same input as
# an Array, ry's unboxed vector, and the faster of the two counts. An Array is the fair
# counterpart of R's vectorized functions, which also work on unboxed storage.
source(commandArgs(trailingOnly = TRUE)[[1]])

# List

case("List.from_vector", setup = { x <- doubles(n) }, ry = List.from_vector(x))
case("List.to_vector", setup = { xs <- List.from_vector(doubles(n)); xsa <- as.Array(doubles(n)) }, ry = List.to_vector(xs), array = List.to_vector(xsa))
case("List.len", setup = { xs <- List.from_vector(doubles(n)); xsa <- as.Array(doubles(n)) }, ry = {
  total <- 0
  for (i in seq_len(n)) total <- total + List.len(xs)
  total
}, array = {
  total <- 0
  for (i in seq_len(n)) total <- total + List.len(xsa)
  total
})
case("List.is_empty", setup = { xs <- List.from_vector(doubles(n)); xsa <- as.Array(doubles(n)) }, ry = {
  total <- 0L
  for (i in seq_len(n)) total <- total + List.is_empty(xs)
  total
}, array = {
  total <- 0L
  for (i in seq_len(n)) total <- total + List.is_empty(xsa)
  total
})
case("List.get", setup = { xs <- List.from_vector(doubles(n)); xsa <- as.Array(doubles(n)); out <- vector("list", n) }, ry = {
  for (i in seq_len(n)) out[[i]] <- List.get(xs, i)
  out
}, array = {
  for (i in seq_len(n)) out[[i]] <- List.get(xsa, i)
  out
})
case("List.set", setup = { xs <- List.from_vector(doubles(n)) }, ry = {
  for (i in seq_len(n)) xs <- List.set(xs, i, 0)
  xs
})
case("List.update", setup = { xs <- List.from_vector(doubles(n)); f <- function(v) v + 1 }, ry = {
  for (i in seq_len(n)) xs <- List.update(xs, i, f)
  xs
})
case("List.append", ry = {
  ys <- list()
  for (i in seq_len(n)) ys <- List.append(ys, i)
  ys
})
case("List.prepend", setup = { xs <- List.from_vector(doubles(n)); xsa <- as.Array(doubles(n)) }, ry = {
  for (k in 1:10) ys <- List.prepend(xs, 0)
  ys
}, array = {
  for (k in 1:10) ys <- List.prepend(xsa, 0)
  ys
})
case("List.concat", setup = { xs <- List.from_vector(doubles(n)); xsa <- as.Array(doubles(n)) }, ry = {
  for (k in 1:10) ys <- List.concat(xs, xs)
  ys
}, array = {
  for (k in 1:10) ys <- List.concat(xsa, xsa)
  ys
})
case("List.single", setup = { out <- vector("list", n) }, ry = {
  for (i in seq_len(n)) out[[i]] <- List.single(i)
  out
})
case("List.repeat", ry = List.repeat(0, n))
case("List.range", ry = List.range(1L, n))
case("List.reverse", setup = { xs <- List.from_vector(doubles(n)); xsa <- as.Array(doubles(n)) }, ry = {
  for (k in 1:10) ys <- List.reverse(xs)
  ys
}, array = {
  for (k in 1:10) ys <- List.reverse(xsa)
  ys
})
case("List.join", setup = { lists <- List.chunks_of(List.from_vector(doubles(n)), 1000L) }, ry = List.join(lists))
case("List.map", setup = { xs <- List.from_vector(doubles(n)); xsa <- as.Array(doubles(n)) }, ry = List.map(xs, function(x) x * 2), array = List.map(xsa, function(x) x * 2))
case("List.map_with_index", setup = { xs <- List.from_vector(doubles(n)); xsa <- as.Array(doubles(n)) }, ry = {
  List.map_with_index(xs, function(x, i) x * i)
}, array = {
  List.map_with_index(xsa, function(x, i) x * i)
})
case("List.map2", setup = { xs <- List.from_vector(doubles(n)); xsa <- as.Array(doubles(n)) }, ry = List.map2(xs, xs, function(a, b) a + b), array = List.map2(xsa, xsa, function(a, b) a + b))
case("List.map3", setup = { xs <- List.from_vector(doubles(n)); xsa <- as.Array(doubles(n)) }, ry = {
  List.map3(xs, xs, xs, function(a, b, c) a + b + c)
}, array = {
  List.map3(xsa, xsa, xsa, function(a, b, c) a + b + c)
})
case("List.map4", setup = { xs <- List.from_vector(doubles(n)); xsa <- as.Array(doubles(n)) }, ry = {
  List.map4(xs, xs, xs, xs, function(a, b, c, d) a + b + c + d)
}, array = {
  List.map4(xsa, xsa, xsa, xsa, function(a, b, c, d) a + b + c + d)
})
case("List.join_map", setup = { xs <- List.from_vector(doubles(n)); xsa <- as.Array(doubles(n)) }, ry = List.join_map(xs, function(x) list(x, x)), array = List.join_map(xsa, function(x) list(x, x)))
case("List.keep_if", setup = { xs <- List.from_vector(doubles(n)); xsa <- as.Array(doubles(n)) }, ry = List.keep_if(xs, function(x) x %% 2 == 0), array = List.keep_if(xsa, function(x) x %% 2 == 0))
case("List.keep_if_try", setup = { xs <- List.from_vector(doubles(n)); xsa <- as.Array(doubles(n)) }, ry = {
  List.keep_if_try(xs, function(x) :Ok(x %% 2 == 0))
}, array = {
  List.keep_if_try(xsa, function(x) :Ok(x %% 2 == 0))
})
case("List.drop_if", setup = { xs <- List.from_vector(doubles(n)); xsa <- as.Array(doubles(n)) }, ry = List.drop_if(xs, function(x) x %% 2 == 0), array = List.drop_if(xsa, function(x) x %% 2 == 0))
case("List.keep_oks", setup = { xs <- List.from_vector(doubles(n)); xsa <- as.Array(doubles(n)) }, ry = {
  List.keep_oks(xs, function(x) if (x %% 2 == 0) :Ok(x) else :Err(:Odd))
}, array = {
  List.keep_oks(xsa, function(x) if (x %% 2 == 0) :Ok(x) else :Err(:Odd))
})
case("List.keep_errs", setup = { xs <- List.from_vector(doubles(n)); xsa <- as.Array(doubles(n)) }, ry = {
  List.keep_errs(xs, function(x) if (x %% 2 == 0) :Ok(x) else :Err(x))
}, array = {
  List.keep_errs(xsa, function(x) if (x %% 2 == 0) :Ok(x) else :Err(x))
})
case("List.map_try", setup = { xs <- List.from_vector(doubles(n)); xsa <- as.Array(doubles(n)) }, ry = {
  List.map_try(xs, function(x) if (x > 0) :Ok(x * 2) else :Err(:NotPositive))
}, array = {
  List.map_try(xsa, function(x) if (x > 0) :Ok(x * 2) else :Err(:NotPositive))
})
case("List.walk", setup = { xs <- List.from_vector(doubles(n)); xsa <- as.Array(doubles(n)) }, ry = List.walk(xs, 0, function(s, x) s + x), array = List.walk(xsa, 0, function(s, x) s + x))
case("List.walk_backwards", setup = { xs <- List.from_vector(doubles(n)); xsa <- as.Array(doubles(n)) }, ry = {
  List.walk_backwards(xs, 0, function(s, x) s + x)
}, array = {
  List.walk_backwards(xsa, 0, function(s, x) s + x)
})
case("List.walk_until", setup = { xs <- List.from_vector(doubles(n)); xsa <- as.Array(doubles(n)); stop_at <- n / 2 }, ry = {
  List.walk_until(xs, 0, function(s, x) if (x > stop_at) :Break(s) else :Continue(s + x))
}, array = {
  List.walk_until(xsa, 0, function(s, x) if (x > stop_at) :Break(s) else :Continue(s + x))
})
case("List.walk_backwards_until", setup = { xs <- List.from_vector(doubles(n)); xsa <- as.Array(doubles(n)); stop_at <- n / 2 }, ry = {
  List.walk_backwards_until(xs, 0, function(s, x) if (x < stop_at) :Break(s) else :Continue(s + x))
}, array = {
  List.walk_backwards_until(xsa, 0, function(s, x) if (x < stop_at) :Break(s) else :Continue(s + x))
})
case("List.walk_with_index", setup = { xs <- List.from_vector(doubles(n)); xsa <- as.Array(doubles(n)) }, ry = {
  List.walk_with_index(xs, 0, function(s, x, i) s + x * i)
}, array = {
  List.walk_with_index(xsa, 0, function(s, x, i) s + x * i)
})
case("List.walk_with_index_until", setup = { xs <- List.from_vector(doubles(n)); xsa <- as.Array(doubles(n)); stop_at <- n %/% 2L }, ry = {
  List.walk_with_index_until(xs, 0, function(s, x, i) if (i > stop_at) :Break(s) else :Continue(s + x * i))
}, array = {
  List.walk_with_index_until(xsa, 0, function(s, x, i) if (i > stop_at) :Break(s) else :Continue(s + x * i))
})
case("List.walk_from", setup = { xs <- List.from_vector(doubles(n)); xsa <- as.Array(doubles(n)); from <- n %/% 2L }, ry = {
  List.walk_from(xs, from, 0, function(s, x) s + x)
}, array = {
  List.walk_from(xsa, from, 0, function(s, x) s + x)
})
case("List.walk_from_until", setup = { xs <- List.from_vector(doubles(n)); xsa <- as.Array(doubles(n)); from <- n %/% 4L; stop_at <- n / 2 }, ry = {
  List.walk_from_until(xs, from, 0, function(s, x) if (x > stop_at) :Break(s) else :Continue(s + x))
}, array = {
  List.walk_from_until(xsa, from, 0, function(s, x) if (x > stop_at) :Break(s) else :Continue(s + x))
})
case("List.walk_try", setup = { xs <- List.from_vector(doubles(n)); xsa <- as.Array(doubles(n)) }, ry = {
  List.walk_try(xs, 0, function(s, x) if (x > 0) :Ok(s + x) else :Err(:NotPositive))
}, array = {
  List.walk_try(xsa, 0, function(s, x) if (x > 0) :Ok(s + x) else :Err(:NotPositive))
})
case("List.for_each", setup = { xs <- List.from_vector(doubles(n)); xsa <- as.Array(doubles(n)) }, ry = {
  total <- 0
  List.for_each(xs, function(x) total <<- total + x)
  total
}, array = {
  total <- 0
  List.for_each(xsa, function(x) total <<- total + x)
  total
})
case("List.for_each_try", setup = { xs <- List.from_vector(doubles(n)); xsa <- as.Array(doubles(n)) }, ry = {
  total <- 0
  List.for_each_try(xs, function(x) if (x > 0) :Ok(total <<- total + x) else :Err(:NotPositive))
  total
}, array = {
  total <- 0
  List.for_each_try(xsa, function(x) if (x > 0) :Ok(total <<- total + x) else :Err(:NotPositive))
  total
})
case("List.any", setup = { xs <- List.from_vector(doubles(n)); xsa <- as.Array(doubles(n)) }, ry = List.any(xs, function(x) x < 0), array = List.any(xsa, function(x) x < 0))
case("List.all", setup = { xs <- List.from_vector(doubles(n)); xsa <- as.Array(doubles(n)) }, ry = List.all(xs, function(x) x > 0), array = List.all(xsa, function(x) x > 0))
case("List.contains", setup = { xs <- List.from_vector(doubles(n)); xsa <- as.Array(doubles(n)) }, ry = {
  found <- 0L
  for (k in 1:10) found <- found + List.contains(xs, -1)
  found
}, array = {
  found <- 0L
  for (k in 1:10) found <- found + List.contains(xsa, -1)
  found
})
case("List.count_if", setup = { xs <- List.from_vector(doubles(n)); xsa <- as.Array(doubles(n)) }, ry = List.count_if(xs, function(x) x %% 3 == 0), array = List.count_if(xsa, function(x) x %% 3 == 0))
case("List.find_first", setup = { xs <- List.from_vector(doubles(n)); xsa <- as.Array(doubles(n)); target <- n / 2 }, ry = {
  List.find_first(xs, function(x) x > target)
}, array = {
  List.find_first(xsa, function(x) x > target)
})
case("List.find_last", setup = { xs <- List.from_vector(doubles(n)); xsa <- as.Array(doubles(n)); target <- n / 2 }, ry = {
  List.find_last(xs, function(x) x < target)
}, array = {
  List.find_last(xsa, function(x) x < target)
})
case("List.find_first_index", setup = { xs <- List.from_vector(doubles(n)); xsa <- as.Array(doubles(n)); target <- n / 2 }, ry = {
  List.find_first_index(xs, function(x) x > target)
}, array = {
  List.find_first_index(xsa, function(x) x > target)
})
case("List.find_last_index", setup = { xs <- List.from_vector(doubles(n)); xsa <- as.Array(doubles(n)); target <- n / 2 }, ry = {
  List.find_last_index(xs, function(x) x < target)
}, array = {
  List.find_last_index(xsa, function(x) x < target)
})
case("List.first", setup = { xs <- List.from_vector(doubles(n)); xsa <- as.Array(doubles(n)); out <- vector("list", n) }, ry = {
  for (i in seq_len(n)) out[[i]] <- List.first(xs)
  out
}, array = {
  for (i in seq_len(n)) out[[i]] <- List.first(xsa)
  out
})
case("List.last", setup = { xs <- List.from_vector(doubles(n)); xsa <- as.Array(doubles(n)); out <- vector("list", n) }, ry = {
  for (i in seq_len(n)) out[[i]] <- List.last(xs)
  out
}, array = {
  for (i in seq_len(n)) out[[i]] <- List.last(xsa)
  out
})
case("List.take_first", setup = { xs <- List.from_vector(doubles(n)); xsa <- as.Array(doubles(n)) }, ry = {
  for (k in 1:10) ys <- List.take_first(xs, n %/% 2L)
  ys
}, array = {
  for (k in 1:10) ys <- List.take_first(xsa, n %/% 2L)
  ys
})
case("List.take_last", setup = { xs <- List.from_vector(doubles(n)); xsa <- as.Array(doubles(n)) }, ry = {
  for (k in 1:10) ys <- List.take_last(xs, n %/% 2L)
  ys
}, array = {
  for (k in 1:10) ys <- List.take_last(xsa, n %/% 2L)
  ys
})
case("List.drop_first", setup = { xs <- List.from_vector(doubles(n)); xsa <- as.Array(doubles(n)) }, ry = {
  for (k in 1:10) ys <- List.drop_first(xs, n %/% 2L)
  ys
}, array = {
  for (k in 1:10) ys <- List.drop_first(xsa, n %/% 2L)
  ys
})
case("List.drop_last", setup = { xs <- List.from_vector(doubles(n)); xsa <- as.Array(doubles(n)) }, ry = {
  for (k in 1:10) ys <- List.drop_last(xs, n %/% 2L)
  ys
}, array = {
  for (k in 1:10) ys <- List.drop_last(xsa, n %/% 2L)
  ys
})
case("List.drop_at", setup = { xs <- List.from_vector(doubles(n)); xsa <- as.Array(doubles(n)) }, ry = {
  for (k in 1:10) ys <- List.drop_at(xs, n %/% 2L)
  ys
}, array = {
  for (k in 1:10) ys <- List.drop_at(xsa, n %/% 2L)
  ys
})
case("List.sublist", setup = { xs <- List.from_vector(doubles(n)); xsa <- as.Array(doubles(n)) }, ry = {
  for (k in 1:10) ys <- List.sublist(xs, n %/% 4L, n %/% 2L)
  ys
}, array = {
  for (k in 1:10) ys <- List.sublist(xsa, n %/% 4L, n %/% 2L)
  ys
})
case("List.split_at", setup = { xs <- List.from_vector(doubles(n)); xsa <- as.Array(doubles(n)) }, ry = {
  for (k in 1:10) parts <- List.split_at(xs, n %/% 2L)
  parts
}, array = {
  for (k in 1:10) parts <- List.split_at(xsa, n %/% 2L)
  parts
})
case("List.split_first", setup = { xs <- List.from_vector(doubles(n)); xsa <- as.Array(doubles(n)); delimiter <- n / 2 }, ry = {
  for (k in 1:10) parts <- List.split_first(xs, delimiter)
  parts
}, array = {
  for (k in 1:10) parts <- List.split_first(xsa, delimiter)
  parts
})
case("List.split_last", setup = { xs <- List.from_vector(doubles(n)); xsa <- as.Array(doubles(n)); delimiter <- n / 2 }, ry = {
  for (k in 1:10) parts <- List.split_last(xs, delimiter)
  parts
}, array = {
  for (k in 1:10) parts <- List.split_last(xsa, delimiter)
  parts
})
case("List.split_on", setup = { xs <- List.from_vector(doubles(n) %% 10); xsa <- as.Array(doubles(n) %% 10) }, ry = List.split_on(xs, 0), array = List.split_on(xsa, 0))
case("List.split_on_list", setup = { xs <- List.from_vector(doubles(n) %% 10); xsa <- as.Array(doubles(n) %% 10) }, ry = {
  List.split_on_list(xs, list(9, 0))
}, array = {
  List.split_on_list(xsa, list(9, 0))
})
case("List.chunks_of", setup = { xs <- List.from_vector(doubles(n)); xsa <- as.Array(doubles(n)) }, ry = List.chunks_of(xs, 10L), array = List.chunks_of(xsa, 10L))
case("List.intersperse", setup = { xs <- List.from_vector(doubles(n)); xsa <- as.Array(doubles(n)) }, ry = List.intersperse(xs, 0), array = List.intersperse(xsa, 0))
case("List.sort_with", setup = { xs <- List.from_vector(scrambled(n)); xsa <- as.Array(scrambled(n)) }, ry = {
  List.sort_with(xs, function(a, b) Num.compare(a, b))
}, array = {
  List.sort_with(xsa, function(a, b) Num.compare(a, b))
})
case("List.sort_asc", setup = { xs <- List.from_vector(scrambled(n)); xsa <- as.Array(scrambled(n)) }, ry = List.sort_asc(xs), array = List.sort_asc(xsa))
case("List.sort_desc", setup = { xs <- List.from_vector(scrambled(n)); xsa <- as.Array(scrambled(n)) }, ry = List.sort_desc(xs), array = List.sort_desc(xsa))
case("List.swap", setup = { xs <- List.from_vector(doubles(n)) }, ry = {
  for (i in seq_len(n %/% 2L)) xs <- List.swap(xs, i, n - i + 1L)
  xs
})
case("List.replace", setup = { xs <- List.from_vector(doubles(n)) }, ry = {
  total <- 0
  for (i in seq_len(n)) {
    replaced <- List.replace(xs, i, 0)
    xs <- replaced$list
    total <- total + replaced$value
  }
  total
})
case("List.append_if_ok", setup = {
  results <- List.map(List.from_vector(doubles(n)), function(x) if (x %% 2 == 0) :Ok(x) else :Err(:Odd))
}, ry = {
  ys <- list()
  for (i in seq_len(n)) ys <- List.append_if_ok(ys, results[[i]])
  ys
})
case("List.prepend_if_ok", setup = { xs <- List.from_vector(doubles(n)); xsa <- as.Array(doubles(n)) }, ry = {
  for (k in 1:10) ys <- List.prepend_if_ok(xs, :Ok(0))
  ys
}, array = {
  for (k in 1:10) ys <- List.prepend_if_ok(xsa, :Ok(0))
  ys
})
case("List.concat_utf8", setup = { bytes <- List.from_vector(seq_len(n) %% 128L) }, ry = {
  for (k in 1:10) ys <- List.concat_utf8(bytes, "abc")
  ys
})
case("List.with_capacity", ry = {
  ys <- List.with_capacity(n)
  for (i in seq_len(n)) ys <- List.append(ys, i)
  ys
})
case("List.reserve", ry = {
  ys <- List.reserve(list(), n)
  for (i in seq_len(n)) ys <- List.append(ys, i)
  ys
})
case("List.release_excess_capacity", setup = { xs <- List.reserve(List.from_vector(doubles(n)), n) }, ry = {
  List.release_excess_capacity(xs)
})
case("List.sum", setup = { xs <- List.from_vector(doubles(n)); xsa <- as.Array(doubles(n)) }, ry = {
  total <- 0
  for (k in 1:10) total <- total + List.sum(xs)
  total
}, array = {
  total <- 0
  for (k in 1:10) total <- total + List.sum(xsa)
  total
})
case("List.product", setup = { xs <- List.from_vector(1 + doubles(n) * 1e-12); xsa <- as.Array(1 + doubles(n) * 1e-12) }, ry = {
  total <- 0
  for (k in 1:10) total <- total + List.product(xs)
  total
}, array = {
  total <- 0
  for (k in 1:10) total <- total + List.product(xsa)
  total
})
case("List.min", setup = { xs <- List.from_vector(scrambled(n)); xsa <- as.Array(scrambled(n)) }, ry = {
  total <- 0
  for (k in 1:10) total <- total + Result.with_default(List.min(xs), 0)
  total
}, array = {
  total <- 0
  for (k in 1:10) total <- total + Result.with_default(List.min(xsa), 0)
  total
})
case("List.max", setup = { xs <- List.from_vector(scrambled(n)); xsa <- as.Array(scrambled(n)) }, ry = {
  total <- 0
  for (k in 1:10) total <- total + Result.with_default(List.max(xs), 0)
  total
}, array = {
  total <- 0
  for (k in 1:10) total <- total + Result.with_default(List.max(xsa), 0)
  total
})
case("List.starts_with", setup = {
  xs <- List.from_vector(doubles(n)); xsa <- as.Array(doubles(n))
  prefix <- List.from_vector(doubles(n %/% 2L))
}, ry = {
  found <- 0L
  for (k in 1:10) found <- found + List.starts_with(xs, prefix)
  found
}, array = {
  found <- 0L
  for (k in 1:10) found <- found + List.starts_with(xsa, prefix)
  found
})
case("List.ends_with", setup = {
  xs <- List.from_vector(doubles(n)); xsa <- as.Array(doubles(n))
  suffix <- List.from_vector(doubles(n)[-seq_len(n %/% 2L)])
}, ry = {
  found <- 0L
  for (k in 1:10) found <- found + List.ends_with(xs, suffix)
  found
}, array = {
  found <- 0L
  for (k in 1:10) found <- found + List.ends_with(xsa, suffix)
  found
})

# Str

case("Str.is_empty", setup = { s <- List.from_vector(words(n)); sa <- as.Array(words(n)) }, ry = List.map(s, Str.is_empty), array = List.map(sa, Str.is_empty))
case("Str.concat", setup = { s <- List.from_vector(words(n)); sa <- as.Array(words(n)) }, ry = List.map2(s, s, Str.concat), array = List.map2(sa, sa, Str.concat))
case("Str.join_with", setup = { s <- List.from_vector(words(n)); sa <- as.Array(words(n)) }, ry = Str.join_with(s, ","), array = Str.join_with(sa, ","))
case("Str.split_on", setup = { joined <- paste(words(n), collapse = ",") }, ry = Str.split_on(joined, ","))
case("Str.repeat", setup = { s <- List.from_vector(words(n)); sa <- as.Array(words(n)) }, ry = List.map(s, function(x) Str.repeat(x, 3L)), array = List.map(sa, function(x) Str.repeat(x, 3L)))
case("Str.starts_with", setup = { s <- List.from_vector(words(n)); sa <- as.Array(words(n)) }, ry = {
  List.map(s, function(x) Str.starts_with(x, "item_00"))
}, array = {
  List.map(sa, function(x) Str.starts_with(x, "item_00"))
})
case("Str.ends_with", setup = { s <- List.from_vector(words(n)); sa <- as.Array(words(n)) }, ry = {
  List.map(s, function(x) Str.ends_with(x, "5_value"))
}, array = {
  List.map(sa, function(x) Str.ends_with(x, "5_value"))
})
case("Str.contains", setup = { s <- List.from_vector(words(n)); sa <- as.Array(words(n)) }, ry = List.map(s, function(x) Str.contains(x, "55")), array = List.map(sa, function(x) Str.contains(x, "55")))
case("Str.trim", setup = { s <- List.from_vector(padded(n)); sa <- as.Array(padded(n)) }, ry = List.map(s, Str.trim), array = List.map(sa, Str.trim))
case("Str.trim_start", setup = { s <- List.from_vector(padded(n)); sa <- as.Array(padded(n)) }, ry = List.map(s, Str.trim_start), array = List.map(sa, Str.trim_start))
case("Str.trim_end", setup = { s <- List.from_vector(padded(n)); sa <- as.Array(padded(n)) }, ry = List.map(s, Str.trim_end), array = List.map(sa, Str.trim_end))
case("Str.drop_prefix", setup = { s <- List.from_vector(words(n)); sa <- as.Array(words(n)) }, ry = {
  List.map(s, function(x) Str.drop_prefix(x, "item_"))
}, array = {
  List.map(sa, function(x) Str.drop_prefix(x, "item_"))
})
case("Str.drop_suffix", setup = { s <- List.from_vector(words(n)); sa <- as.Array(words(n)) }, ry = {
  List.map(s, function(x) Str.drop_suffix(x, "_value"))
}, array = {
  List.map(sa, function(x) Str.drop_suffix(x, "_value"))
})
case("Str.with_prefix", setup = { s <- List.from_vector(words(n)); sa <- as.Array(words(n)) }, ry = List.map(s, function(x) Str.with_prefix(x, "my_")), array = List.map(sa, function(x) Str.with_prefix(x, "my_")))
case("Str.replace_each", setup = { s <- List.from_vector(words(n)); sa <- as.Array(words(n)) }, ry = {
  List.map(s, function(x) Str.replace_each(x, "_", "-"))
}, array = {
  List.map(sa, function(x) Str.replace_each(x, "_", "-"))
})
case("Str.replace_first", setup = { s <- List.from_vector(words(n)); sa <- as.Array(words(n)) }, ry = {
  List.map(s, function(x) Str.replace_first(x, "_", "-"))
}, array = {
  List.map(sa, function(x) Str.replace_first(x, "_", "-"))
})
case("Str.replace_last", setup = { s <- List.from_vector(words(n)); sa <- as.Array(words(n)) }, ry = {
  List.map(s, function(x) Str.replace_last(x, "_", "-"))
}, array = {
  List.map(sa, function(x) Str.replace_last(x, "_", "-"))
})
case("Str.split_first", setup = { s <- List.from_vector(words(n)); sa <- as.Array(words(n)) }, ry = List.map(s, function(x) Str.split_first(x, "_")), array = List.map(sa, function(x) Str.split_first(x, "_")))
case("Str.split_last", setup = { s <- List.from_vector(words(n)); sa <- as.Array(words(n)) }, ry = List.map(s, function(x) Str.split_last(x, "_")), array = List.map(sa, function(x) Str.split_last(x, "_")))
case("Str.count_utf8_bytes", setup = { s <- List.from_vector(words(n)); sa <- as.Array(words(n)) }, ry = List.map(s, Str.count_utf8_bytes), array = List.map(sa, Str.count_utf8_bytes))
case("Str.to_utf8", setup = { s <- List.from_vector(words(n)); sa <- as.Array(words(n)) }, ry = List.map(s, Str.to_utf8), array = List.map(sa, Str.to_utf8))
case("Str.from_utf8", setup = { bytes <- List.map(List.from_vector(words(n)), Str.to_utf8) }, ry = {
  List.map(bytes, Str.from_utf8)
})
case("Str.from_utf8_lossy", setup = { bytes <- List.map(List.from_vector(words(n)), Str.to_utf8) }, ry = {
  List.map(bytes, Str.from_utf8_lossy)
})
case("Str.to_integer", setup = { s <- List.from_vector(as.character(seq_len(n))); sa <- as.Array(as.character(seq_len(n))) }, ry = List.map(s, Str.to_integer), array = List.map(sa, Str.to_integer))
case("Str.to_double", setup = { s <- List.from_vector(as.character(seq_len(n) / 4)); sa <- as.Array(as.character(seq_len(n) / 4)) }, ry = List.map(s, Str.to_double), array = List.map(sa, Str.to_double))
case("Str.with_ascii_lowercased", setup = { s <- List.from_vector(toupper(words(n))); sa <- as.Array(toupper(words(n))) }, ry = {
  List.map(s, Str.with_ascii_lowercased)
}, array = {
  List.map(sa, Str.with_ascii_lowercased)
})
case("Str.with_ascii_uppercased", setup = { s <- List.from_vector(words(n)); sa <- as.Array(words(n)) }, ry = List.map(s, Str.with_ascii_uppercased), array = List.map(sa, Str.with_ascii_uppercased))
case("Str.caseless_ascii_equals", setup = {
  s <- List.from_vector(words(n)); sa <- as.Array(words(n))
  upper <- List.from_vector(toupper(words(n)))
}, ry = List.map2(s, upper, Str.caseless_ascii_equals), array = List.map2(sa, upper, Str.caseless_ascii_equals))
case("Str.walk_utf8", setup = { text <- strrep("abcdefghij", n %/% 10L) }, ry = {
  Str.walk_utf8(text, 0L, function(s, byte) s + byte)
})
case("Str.walk_utf8_with_index", setup = { text <- strrep("abcdefghij", n %/% 10L) }, ry = {
  Str.walk_utf8_with_index(text, 0, function(s, byte, i) s + byte * i)
})
case("Str.to_utf16", setup = { s <- List.from_vector(words(n)); sa <- as.Array(words(n)) }, ry = List.map(s, Str.to_utf16), array = List.map(sa, Str.to_utf16))
case("Str.from_utf16", setup = { units <- List.map(List.from_vector(words(n)), Str.to_utf16) }, ry = {
  List.map(units, Str.from_utf16)
})
case("Str.from_utf16_lossy", setup = { units <- List.map(List.from_vector(words(n)), Str.to_utf16) }, ry = {
  List.map(units, Str.from_utf16_lossy)
})
case("Str.to_utf32", setup = { s <- List.from_vector(words(n)); sa <- as.Array(words(n)) }, ry = List.map(s, Str.to_utf32), array = List.map(sa, Str.to_utf32))
case("Str.from_utf32", setup = { points <- List.map(List.from_vector(words(n)), Str.to_utf32) }, ry = {
  List.map(points, Str.from_utf32)
})
case("Str.from_utf32_lossy", setup = { points <- List.map(List.from_vector(words(n)), Str.to_utf32) }, ry = {
  List.map(points, Str.from_utf32_lossy)
})

# Dict. `filled` builds a dictionary from `k1` to `kn`, each key's value its number.

filled <- function(count) {
  pairs <- List.map2(List.from_vector(keys(count)), List.from_vector(doubles(count)), function(k, v) as.tuple(list(k, v)))
  Dict.from_list(pairs)
}

case("Dict.empty", ry = {
  for (i in seq_len(n)) d <- Dict.empty()
  d
})
case("Dict.with_capacity", setup = { k <- keys(n) }, ry = {
  d <- Dict.with_capacity(n)
  for (i in seq_len(n)) d <- Dict.insert(d, k[[i]], i)
  d
})
case("Dict.single", setup = { out <- vector("list", n) }, ry = {
  for (i in seq_len(n)) out[[i]] <- Dict.single("key", i)
  out
})
case("Dict.from_list", setup = {
  pairs <- List.map2(List.from_vector(keys(n)), List.from_vector(doubles(n)), function(k, v) as.tuple(list(k, v)))
}, ry = Dict.from_list(pairs))
case("Dict.to_list", setup = { d <- filled(n) }, ry = Dict.to_list(d))
case("Dict.len", setup = { d <- filled(1000L) }, ry = {
  total <- 0
  for (i in seq_len(n)) total <- total + Dict.len(d)
  total
})
case("Dict.is_empty", setup = { d <- filled(1000L) }, ry = {
  total <- 0L
  for (i in seq_len(n)) total <- total + Dict.is_empty(d)
  total
})
case("Dict.get", setup = { d <- filled(n); k <- rev(keys(n)); out <- vector("list", n) }, ry = {
  for (i in seq_len(n)) out[[i]] <- Dict.get(d, k[[i]])
  out
})
case("Dict.contains", setup = { d <- filled(n); k <- rev(keys(n)) }, ry = {
  found <- 0L
  for (i in seq_len(n)) found <- found + Dict.contains(d, k[[i]])
  found
})
case("Dict.insert", setup = { k <- keys(n) }, ry = {
  d <- Dict.empty()
  for (i in seq_len(n)) d <- Dict.insert(d, k[[i]], i)
  d
})
case("Dict.remove", setup = { d <- filled(n); k <- keys(n) }, ry = {
  for (i in seq_len(n %/% 2L)) d <- Dict.remove(d, k[[i]])
  d
})
case("Dict.update", setup = { w <- keys(1000L)[seq_len(n) %% 1000L + 1L] }, ry = {
  counts <- Dict.empty()
  for (i in seq_len(n)) {
    counts <- Dict.update(counts, w[[i]], function(found) :Ok(Result.with_default(found, 0L) + 1L))
  }
  counts
})
case("Dict.keys", setup = { d <- filled(n) }, ry = Dict.keys(d))
case("Dict.values", setup = { d <- filled(n) }, ry = Dict.values(d))
case("Dict.walk", setup = { d <- filled(n) }, ry = Dict.walk(d, 0, function(s, k, v) s + v))
case("Dict.walk_until", setup = { d <- filled(n); stop_at <- n / 2 }, ry = {
  Dict.walk_until(d, 0, function(s, k, v) if (v > stop_at) :Break(s) else :Continue(s + v))
})
case("Dict.map", setup = { d <- filled(n) }, ry = Dict.map(d, function(k, v) v * 2))
case("Dict.join_map", setup = { d <- filled(n) }, ry = Dict.join_map(d, function(k, v) Dict.single(k, v + 1)))
case("Dict.keep_if", setup = { d <- filled(n) }, ry = Dict.keep_if(d, function(k, v) v %% 2 == 0))
case("Dict.drop_if", setup = { d <- filled(n) }, ry = Dict.drop_if(d, function(k, v) v %% 2 == 0))
case("Dict.insert_all", setup = { first <- filled(n); second <- filled(n %/% 2L) }, ry = Dict.insert_all(first, second))
case("Dict.keep_shared", setup = { first <- filled(n); second <- filled(n %/% 2L) }, ry = Dict.keep_shared(first, second))
case("Dict.remove_all", setup = { first <- filled(n); second <- filled(n %/% 2L) }, ry = Dict.remove_all(first, second))
case("Dict.reserve", setup = { k <- keys(n) }, ry = {
  d <- Dict.reserve(Dict.empty(), n)
  for (i in seq_len(n)) d <- Dict.insert(d, k[[i]], i)
  d
})
case("Dict.capacity", setup = { d <- filled(1000L) }, ry = {
  total <- 0
  for (i in seq_len(n)) total <- total + Dict.capacity(d)
  total
})
case("Dict.release_excess_capacity", setup = { d <- Dict.reserve(filled(n), n) }, ry = Dict.release_excess_capacity(d))
case("Dict.clear", setup = { d <- filled(n) }, ry = Dict.clear(d))

# Result. Every case maps over n results, half `:Ok(x)` for an even x and half `:Err(:Odd)`.

results <- function(count) {
  List.map(List.from_vector(doubles(count)), function(x) if (x %% 2 == 0) :Ok(x) else :Err(:Odd))
}

case("Result.is_ok", setup = { rs <- results(n) }, ry = List.map(rs, Result.is_ok))
case("Result.is_err", setup = { rs <- results(n) }, ry = List.map(rs, Result.is_err))
case("Result.with_default", setup = { rs <- results(n) }, ry = List.map(rs, function(r) Result.with_default(r, 0)))
case("Result.map", setup = { rs <- results(n) }, ry = List.map(rs, function(r) Result.map(r, function(x) x * 2)))
case("Result.map_err", setup = { rs <- results(n) }, ry = List.map(rs, function(r) Result.map_err(r, function(e) "odd")))
case("Result.map_both", setup = { rs <- results(n) }, ry = {
  List.map(rs, function(r) Result.map_both(r, function(x) x * 2, function(e) "odd"))
})
case("Result.map2", setup = { rs <- results(n) }, ry = {
  List.map2(rs, rs, function(a, b) Result.map2(a, b, function(x, y) x + y))
})
case("Result.and_then", setup = { rs <- results(n); half <- n / 2 }, ry = {
  List.map(rs, function(r) Result.and_then(r, function(x) if (x > half) :Ok(x) else :Err(:Small)))
})
case("Result.on_err", setup = { rs <- results(n) }, ry = List.map(rs, function(r) Result.on_err(r, function(e) :Ok(0))))
