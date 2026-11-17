# A failed allocation reports the size of the vector's data, whatever else the runtime stores
# with a vector. Every size here is beyond what a machine can hold.
report <- function(expression) {
  cat(tryCatch({
    expression
    "no error"
  }, error = function(failure) conditionMessage(failure)), "\n")
}

report(vector("list", 1e12))
report(character(1e12))
report(numeric(1e12))
report(integer(1e12))
report(logical(1e12))
report(complex(1e12))
report(raw(1e13))

for (start in list(letters, list(1, "a"), c(1.5, 2), 1:3, TRUE, as.raw(1))) {
  grown <- start
  report(grown[1e12] <- start[1])
  grown <- start
  report(grown[[1e12]] <- start[[1]])
  grown <- start
  report(length(grown) <- 1e12)
}

integers <- 1:3
report(integers[1e12] <- "a")
report(rep(1, 1e12))
report(rep(list(1), 1e12))
report(seq_len(1e12) + 1L)

# A length outside the integer range, missing or negative is refused before anything is
# allocated.
show <- function(expression) {
  tryCatch({
    value <- expression
    cat(typeof(value), length(value), "\n")
  },
  warning = function(caught) cat("warning:", conditionMessage(caught), "\n"),
  error = function(failure) cat("error:", conditionMessage(failure), "\n"))
}
show(complex(3e9))
show(complex(2^31))
show(complex(-1))
show(complex(NA))
show(complex(NULL))
show(complex(2.7))
show(complex("3"))
show(complex(c(2, 3)))
show(complex(TRUE))
show(complex(real = 1:3, length.out = 2))
show(complex(length.out = 1e12, real = 1))
print(withCallingHandlers(tryCatch(complex(1e12), error = conditionMessage), warning = function(caught) {
  cat("warning:", conditionMessage(caught), "\n")
  invokeRestart("muffleWarning")
}))
