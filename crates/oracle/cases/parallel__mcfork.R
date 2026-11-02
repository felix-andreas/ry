# Forked workers: mclapply and mcparallel run code in child processes and send the results
# back over pipes, with RNG streams that make the results reproducible.
library(parallel)
print(mclapply(1:5, function(i) i * 10L, mc.cores = 2))
parent <- Sys.getpid()
print(unlist(mclapply(1:4, function(i) Sys.getpid() != parent, mc.cores = 2,
                      mc.preschedule = FALSE)))
print(unlist(mclapply(1:4, function(i) Sys.getpid() != parent, mc.cores = 2)))

result <- withCallingHandlers(
  mclapply(1:3, function(i) if (i == 2) stop("boom") else i, mc.cores = 2),
  warning = function(w) {
    print(conditionMessage(w))
    invokeRestart("muffleWarning")
  }
)
print(inherits(result[[2]], "try-error"))
print(result[c(1, 3)])

job <- mcparallel(sum(1:100), name = "total")
other <- mcparallel({ x <- rnorm(3); length(x) })
collected <- mccollect(list(job, other))
print(names(collected)[1])
print(unname(collected))
print(length(parallel:::children()))
print(parallel:::isChild())

RNGkind("L'Ecuyer-CMRG")
set.seed(1)
first <- mclapply(1:3, function(i) runif(1), mc.cores = 2)
set.seed(1)
second <- mclapply(1:3, function(i) runif(1), mc.cores = 2)
print(identical(first, second))

collected <- mccollect(mcparallel(Sys.sleep(0.1)), wait = FALSE, timeout = 5)
print(length(collected))
job <- mcparallel(1, name = "a")
print(names(mccollect(job)) == job$pid)
