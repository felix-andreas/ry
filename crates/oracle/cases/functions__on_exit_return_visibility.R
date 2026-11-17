# A return() inside on.exit replaces the function's value and its visibility.
quiet <- function(x) invisible(x)
f <- function() { on.exit(return(quiet("replaced"))); 1 }
g <- function() { on.exit(return(invisible(2))); 1 }
h <- function() { on.exit(return(3)); invisible(1) }
withVisible(f())
withVisible(g())
withVisible(h())
f()
g()
h()

# Exit code that does not return keeps the visibility of the body's value.
k <- function() { on.exit(invisible(3)); 1 }
m <- function() { on.exit(3); invisible(1) }
withVisible(k())$visible
withVisible(m())$visible

# Several exit expressions: the last return() decides, and later plain ones keep it.
n <- function() {
    on.exit(return(invisible("first")))
    on.exit(return("second"), add = TRUE)
    on.exit(invisible("third"), add = TRUE)
    "body"
}
withVisible(n())
p <- function() {
    on.exit(return("first"))
    on.exit(return(invisible("second")), add = TRUE)
    on.exit(print("third runs"), add = TRUE)
    "body"
}
withVisible(p())
p()

# The same from compiled closures called repeatedly.
for (round in 1:3) {
    print(c(withVisible(f())$visible, withVisible(g())$visible, withVisible(h())$visible, withVisible(n())$visible))
}

# A return() in exit code over an error.
q <- function() { on.exit(return(invisible("recovered"))); stop("failed") }
withVisible(q())
