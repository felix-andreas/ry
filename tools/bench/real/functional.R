# Functional style: lapply, vapply, Map, Filter, Reduce and do.call over lists, the same through
# purrr, function factories and closures that capture state, memoization in an environment, and
# recursion over nested lists such as parsed JSON.
make_scaler <- function(factor, offset = 0) function(x) x * factor + offset
compose <- function(...) {
  fs <- list(...)
  function(x) Reduce(function(acc, f) f(acc), fs, x)
}

memo_env <- new.env(parent = emptyenv())
fib_memo <- function(n) {
  key <- as.character(n)
  if (!is.null(memo_env[[key]])) return(memo_env[[key]])
  value <- if (n < 2) n else fib_memo(n - 1) + fib_memo(n - 2)
  memo_env[[key]] <- value
  value
}

build_tree <- function(depth, breadth, label = "root") {
  if (depth == 0L) return(list(label = label, value = nchar(label)))
  list(label = label, children = lapply(seq_len(breadth), function(i) build_tree(depth - 1L, breadth, paste0(label, ".", i))))
}

tree_sum <- function(node) {
  if (is.null(node$children)) return(node$value)
  sum(vapply(node$children, tree_sum, numeric(1)))
}

tree_labels <- function(node) {
  if (is.null(node$children)) return(node$label)
  unlist(lapply(node$children, tree_labels), use.names = FALSE)
}

functional <- function(n) {
  pipeline <- compose(make_scaler(2), make_scaler(0.5, 1), function(x) x^2, sqrt)
  values <- vapply(seq_len(n), pipeline, numeric(1))
  pairs <- Map(function(a, b, scale = 2) a * scale + b, seq_len(n), rev(seq_len(n)))
  evens <- Filter(function(x) x %% 2 == 0, unlist(pairs))
  folded <- Reduce(`+`, evens, accumulate = FALSE)
  running <- Reduce(function(a, b) a + b, seq_len(200), accumulate = TRUE)
  args <- lapply(seq_len(n %/% 10L), function(i) list(i, i + 1, na.rm = TRUE))
  sums <- vapply(args, function(a) do.call(sum, a), numeric(1))
  mapped <- purrr::map_dbl(seq_len(n), function(x) x / 3)
  chars <- purrr::map_chr(seq_len(n %/% 5L), ~ paste0("item", .x))
  kept <- purrr::keep(seq_len(n), function(x) x %% 3 == 0)
  reduced <- purrr::reduce(seq_len(500), function(a, b) a * 1.0001 + b)
  indexed <- purrr::imap(c(a = 1, b = 2, c = 3), function(x, name) paste(name, x))
  rows <- purrr::pmap_dbl(list(seq_len(n %/% 5L), seq_len(n %/% 5L), 2), function(a, b, c) a * b / c)
  rm(list = ls(memo_env), envir = memo_env)
  fibs <- vapply(seq_len(60), fib_memo, numeric(1))
  tree <- build_tree(6L, 4L)
  list(
    values = sum(values), evens = length(evens), folded = folded, running = running[[200]],
    sums = sum(sums), mapped = sum(mapped), chars = chars[[length(chars)]], kept = length(kept),
    reduced = reduced, indexed = unlist(indexed), rows = sum(rows), fibs = fibs[[60]],
    tree = tree_sum(tree), labels = length(tree_labels(tree))
  )
}

measure("functional", function() functional(40000L))
