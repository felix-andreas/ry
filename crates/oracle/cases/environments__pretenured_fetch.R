# A lazy-load fetch allocates what it reads in the old generation, so a fetched environment is old
# before any collection has run. The case writes its own lazy-load database, fetches an
# environment from it, and stores freshly made, young values into that environment and into the
# fetched objects inside it; the collections between rounds must keep them. Under
# `RY_GC_VERIFY=1` every collection also checks that no old object holds a young one the
# remembered set misses.
source_env <- new.env()
source_env$store <- new.env()
source_env$store$inner <- new.env()
source_env$store$seed <- list(a = 1:3, b = "seed")
filebase <- file.path(tempdir(), "pretenured")
invisible(tools:::makeLazyLoadDB(source_env, filebase))
fetched <- new.env()
invisible(lazyLoad(filebase, envir = fetched))
store <- fetched$store
young <- function(i) list(id = i, label = paste0("v", i), data = seq_len(50) + i, env = new.env())
churn <- function(n) for (i in seq_len(n)) list(i, as.character(i), c(i, i))
for (round in 1:12) {
  assign(paste0("value_", round), young(round), envir = store)
  assign(paste0("inner_", round), paste0("w", round), envir = store$inner)
  assign("value", round * 10, envir = get(paste0("value_", round), envir = store)$env)
  churn(3000)
  if (round %% 4 == 0) invisible(gc())
}
total <- 0
for (round in 1:12) {
  value <- get(paste0("value_", round), envir = store)
  total <- total + value$id + sum(value$data) + nchar(value$label) +
    get("value", envir = value$env) + nchar(get(paste0("inner_", round), envir = store$inner))
}
print(total)
print(store$seed)
print(length(ls(store)))
