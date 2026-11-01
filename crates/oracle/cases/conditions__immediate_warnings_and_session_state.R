# An immediate warning is still a condition: handlers see it before the default handler prints.
r <- tryCatch(warning(immediate. = TRUE, call. = FALSE, "boom"), warning = function(w) conditionMessage(w))
print(r)
f <- function() warning("inner", immediate. = TRUE)
withCallingHandlers(f(), warning = function(w) {
  cat("caught:", conditionMessage(w), "\n")
  invokeRestart("muffleWarning")
})

# options(error = f) stores a call to f, not f itself.
old <- options(error = function() NULL)
print(is.call(getOption("error")))
options(old)

# A pairlist on the right of `[<-` is stored as a list.
x <- list(1, 2)
x[1:2] <- pairlist(a = 3, b = 4)
str(x)

# Sys.glob sorts within each pattern and keeps the patterns in order.
dir <- tempfile()
dir.create(dir)
for (name in c("b.R", "a.R", "c.txt", "sub")) {
  path <- file.path(dir, name)
  if (name == "sub") dir.create(path) else file.create(path)
}
print(basename(Sys.glob(file.path(dir, c("*.txt", "*.R")))))
print(basename(Sys.glob(file.path(dir, "s*"), dirmark = TRUE)))
print(length(Sys.glob(file.path(dir, "nothing*"))))

# Setting LC_COLLATE to C switches sort() to byte order.
old_collate <- Sys.getlocale("LC_COLLATE")
invisible(Sys.setlocale("LC_COLLATE", "C"))
print(Sys.getlocale("LC_COLLATE"))
print(sort(c("b", "A", "a", "B")))
invisible(Sys.setlocale("LC_COLLATE", old_collate))
print(suppressWarnings(Sys.setlocale("LC_COLLATE", "no_such_locale")))
