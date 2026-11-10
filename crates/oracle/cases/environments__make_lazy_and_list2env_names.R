# list2env and delayedAssign-style lazy bindings install names straight from character vectors.
e <- list2env(list(alpha = 1, beta = "b", été = TRUE))
print(sort(ls(e))); print(e$été)
print(tryCatch(list2env(list(1, 2), envir = new.env()), error = conditionMessage))
print(tryCatch(list2env(list(a = 1, 2)), error = conditionMessage))
e3 <- new.env(); invisible(list2env(list(x = 1, x = 2), envir = e3)); print(e3$x)
print(ls(list2env(list(), new.env())))
nm <- c("lazy_a", "lazy_b"); env <- new.env()
for (n in nm) eval(substitute(delayedAssign(N, { cat("forcing", N, "\n"); nchar(N) }, assign.env = env), list(N = n)))
print(env$lazy_b); print(env$lazy_b); print(env$lazy_a)
x <- as.environment(list(first = 1L, second = 2L)); print(mget(c("first", "second"), envir = x))
# An NA name binds `NA`; eval over a list skips an empty name instead of rejecting it.
print(ls(list2env(setNames(list(1), NA_character_))))
print(evalq(x, setNames(list(1, 5), c("", "x"))))
