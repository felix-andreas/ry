# A compiled `pkg::name` reads the namespace binding it found last time, and must see every
# change that would change what the lookup finds.
f <- function() tools::file_ext("a.txt")
g <- function() tools:::file_ext("b.R")
h <- function() base::paste0("x", 1)
k <- function() "tools"::"file_ext"("c.csv")
for (i in 1:3) cat(f(), g(), h(), k(), "\n")

# Assigning the binding in the namespace is seen at once.
ns <- asNamespace("tools")
old <- get("file_ext", ns)
unlockBinding("file_ext", ns)
assign("file_ext", function(x) "patched", envir = ns)
for (i in 1:2) cat(f(), g(), k(), "\n")
assign("file_ext", old, envir = ns)
lockBinding("file_ext", ns)
for (i in 1:2) cat(f(), g(), k(), "\n")

# A local `::` shadows the operator.
m <- function() { `::` <- function(a, b) toupper; tools::file_ext("d.txt") }
for (i in 1:2) cat(m(), f(), "\n")

# An export missing from the table is an error every time, and lazy data is found.
n <- function() tryCatch(tools::no_such_name, error = function(e) conditionMessage(e))
for (i in 1:2) cat(n(), "\n")
d <- function() nrow(datasets::mtcars)
for (i in 1:2) cat(d(), "\n")

# A namespace loaded on first use.
q <- function() splines::bs
for (i in 1:2) cat(is.function(q()), "\n")
