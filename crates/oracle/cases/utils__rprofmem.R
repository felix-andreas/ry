# Rprofmem logs each vector allocation above the threshold with the closures on the stack,
# innermost first. Vectors of up to 128 data bytes never appear, because GNU R takes them
# from pages of small cells.
invisible(compiler::enableJIT(0))
log <- tempfile()
# GNU R also logs some of its own allocations and its "new page:" lines, which depend on the
# state of the collector, so only the allocations made by named functions are compared.
logged <- function() grep('^[0-9]+ :"(numeric|integer|raw|vector)"', readLines(log), value = TRUE)
make <- function(n) numeric(n)
wrap <- function(n) make(n)

allocate <- function() {
  Rprofmem(log, threshold = 1000)
  small <- numeric(16)
  a <- wrap(200)
  b <- integer(300)
  r <- raw(1200)
  x <- 1:2000 + 0
  y <- vector("list", 200)
  Rprofmem(NULL)
}
allocate()
print(logged())
print("16048 :\"allocate\" " %in% readLines(log))

# The threshold is strict: a vector of exactly the threshold's size is not logged.
Rprofmem(log, threshold = 8048)
a <- numeric(1000)
b <- numeric(1001)
Rprofmem(NULL)
print(logged())

# append = TRUE keeps the earlier lines.
Rprofmem(log, append = TRUE, threshold = 1e6)
z <- numeric(2e5)
Rprofmem(NULL)
print(logged())

# An Rprof interval below 10 milliseconds is raised to it with a warning.
profile <- tempfile()
withCallingHandlers(
  Rprof(profile, interval = 0.005),
  warning = function(w) {
    print(conditionMessage(w))
    invokeRestart("muffleWarning")
  }
)
Rprof(NULL)
print(readLines(profile)[1])
Rprof(profile, interval = 0.015)
Rprof(NULL)
print(readLines(profile)[1])
unlink(c(log, profile))
