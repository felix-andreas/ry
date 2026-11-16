# ifelse() with a one-element test takes the R body's quick return natively. The result, the
# branch that is forced, warnings, errors and what argument code sees of the stack match the R
# body, including where the quick return does not apply and the R body runs.
show <- function(value) { print(value); invisible(value) }
attempt <- function(expr) tryCatch(show(expr), error = function(e) cat("error:", conditionMessage(e), "\n"))
for (i in 1:3) {
  show(ifelse(TRUE, 1, 2))
  show(ifelse(FALSE, "a", "b"))
  show(ifelse(NA, 1, 2))
  show(ifelse(c(TRUE, FALSE, NA), 1:3, -(1:3)))
  show(ifelse(logical(), 1, 2))
}

# Tests of other types: numbers, NaN, complex, raw, strings that do and do not spell a logical.
show(ifelse(2L, "yes", "no"))
show(ifelse(0, "yes", "no"))
show(ifelse(NaN, "yes", "no"))
show(ifelse(NA_integer_, "yes", "no"))
show(ifelse(0+1i, "yes", "no"))
show(ifelse(as.raw(0), "yes", "no"))
show(ifelse("T", "yes", "no"))
show(ifelse("false", "yes", "no"))
show(ifelse("abc", "yes", "no"))
show(ifelse(NA_character_, "yes", "no"))
show(ifelse(" TRUE", "yes", "no"))

# Tests with attributes, objects and non-atomic tests take the long path.
show(ifelse(c(a = TRUE), 1, 2))
show(ifelse(matrix(FALSE), 1, 2))
attempt(ifelse(factor("a"), 1, 2))
attempt(ifelse(structure(TRUE, class = "flag"), 1, 2))
attempt(ifelse(list(TRUE), 1, 2))
attempt(ifelse(NULL, 1, 2))
show(tryCatch(ifelse(quote(x), 1, 2), error = conditionMessage))
show(tryCatch(ifelse(sum, 1, 2), error = conditionMessage))
setClass("Truth", representation("logical"))
attempt(ifelse(new("Truth", TRUE), 1, 2))
show(ifelse(Sys.Date() > as.Date("2000-01-01"), "later", "earlier"))
attempt(ifelse(TRUE, as.Date("2026-01-01"), 0))

# Branches with attributes, names, functions, NULL and zero length.
show(ifelse(TRUE, c(a = 1), 2))
show(ifelse(FALSE, 1, c(b = 2)))
attempt(ifelse(TRUE, factor("lvl"), 2))
show(ifelse(TRUE, 1:3, 0))
attempt(ifelse(TRUE, NULL, 0))
attempt(ifelse(TRUE, numeric(), 0))
attempt(ifelse(TRUE, list(1), 0))
attempt(ifelse(TRUE, list(1, 2), 0))
attempt(ifelse(TRUE, sum, 0))
show(tryCatch(ifelse(FALSE, 0, function(x) x), error = conditionMessage))
withSource <- eval(parse(text = "function(x) x + 1", keep.source = TRUE))
show(names(attributes(withSource)))
attempt(ifelse(TRUE, withSource, 0))
show(tryCatch(ifelse(TRUE, structure(function() 1, note = "n"), 0), error = conditionMessage))
attempt(ifelse(TRUE, quote(sym), 0))
attempt(ifelse(TRUE, new.env(), 0))
show(ifelse(TRUE, 1, stop("never forced")))
show(ifelse(FALSE, stop("never forced"), 2))
show(ifelse(NA, stop("never forced"), stop("never forced")))

# Only the chosen branch is forced, and each argument once.
count <- 0
side <- function(value) { count <<- count + 1; cat("forced", value, "\n"); value }
for (i in 1:3) show(ifelse(i > 1, side("yes"), side("no")))
show(count)
show(ifelse(side(TRUE), side(1), side(2)))
show(ifelse(side(c(TRUE, FALSE)), side(1), side(2)))
show(count)

# Warnings and errors come from the same calls.
show(withCallingHandlers(ifelse({ warning("in test"); TRUE }, 1, 2),
  warning = function(w) { print(conditionCall(w)); invokeRestart("muffleWarning") }))
show(tryCatch(ifelse(TRUE, stop("in yes"), 2), error = function(e) conditionMessage(e)))
show(tryCatch(ifelse(TRUE), error = conditionMessage))
show(tryCatch(ifelse(FALSE, 1), error = conditionMessage))
show(tryCatch(ifelse(NA), error = conditionMessage))
show(tryCatch(ifelse(), error = conditionMessage))
f <- function() ifelse(stop("in test"), 1, 2)
show(tryCatch(f(), error = function(e) deparse(conditionCall(e))))
show(as.integer(1:3 > 1))

# Argument code sees ifelse()'s frame, as it does under the R body.
depth <- function() sys.calls()
calls <- ifelse(TRUE, list(depth()), 0)[[1]]
show(vapply(calls, function(call) deparse(call)[[1]], ""))
who <- function() deparse(sys.call(-1))
show(ifelse(TRUE, who(), "no"))
nf <- function() sys.nframe()
show(ifelse(TRUE, nf(), 0) - nf())
show(ifelse(FALSE, 0, nf()) - nf())
g <- function(a, b) ifelse(missing(b), "b missing", "b given")
show(g(1))
show(g(1, 2))
h <- function() ifelse(TRUE, list(parent.frame()), 0)[[1]]
show(identical(h(), environment()))

# Named and reordered arguments, and visibility.
show(ifelse(no = "n", yes = "y", test = FALSE))
show(ifelse(yes = "y", FALSE, "n"))
show(withVisible(ifelse(TRUE, 1, 2)))
show(withVisible(ifelse(NA, 1, 2)))
show(withVisible(ifelse(TRUE, invisible(1), 2)))

# A user's ifelse() masks base's, and an edited copy of base's runs its own body.
local({
  ifelse <- function(test, yes, no) "mine"
  show(ifelse(TRUE, 1, 2))
})
edited <- base::ifelse
body(edited) <- quote("edited")
show(edited(TRUE, 1, 2))
