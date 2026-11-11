# loadNamespace() binds a package's registered native routines in its namespace, under the
# registration prefix, and records them in the namespace information.
routines <- getNamespaceInfo("tools", "nativeRoutines")
names(routines)
r <- routines[["tools"]]
class(r)
length(r) > 10
head(sort(names(r)), 3)
all(startsWith(names(r), "C_"))
all(paste0("C_", r) == names(r))
info <- get(names(r)[1], envir = asNamespace("tools"))
class(info)
names(info)
identical(info$name, unname(r[1]))
stats_routines <- getNamespaceInfo("stats", "nativeRoutines")[["stats"]]
all(paste0("C_", stats_routines) == names(stats_routines))
class(stats:::C_cor)
stats:::C_cor$name
