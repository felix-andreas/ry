# A function bound in the calling frame at run time, in ways the bytecode compiler cannot see,
# must shadow the global one once the call site has cached the global binding.
g <- function() "global"

setter <- function() assign("g", function() "local", envir = parent.frame())
via_parent <- function(set) { if (set) setter(); g() }
for (set in c(FALSE, FALSE, FALSE, TRUE)) print(via_parent(set))

via_default <- function(set, x = if (set) (g <- function() "local")) { x; g() }
for (set in c(FALSE, FALSE, FALSE, TRUE)) print(via_default(set))

via_qualified <- function(set) { if (set) base::assign("g", function() "local"); g() }
for (set in c(FALSE, FALSE, FALSE, TRUE)) print(via_qualified(set))

gen <- function(x, set) { if (set) g <- function() "local"; UseMethod("gen") }
gen.default <- function(x, set) g()
for (set in c(FALSE, FALSE, FALSE, TRUE)) print(gen(1, set))
