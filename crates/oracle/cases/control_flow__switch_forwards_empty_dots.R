# `switch(x, ...)` names its alternatives, so it does not warn when the dots are empty.
forward <- function(x, ...) switch(x, ...)
print(forward("a", a = 1))
print(forward("a"))
print(forward(1))
hot <- function(x, ...) { total <- 0; for (i in 1:200) total <- total + is.null(switch(x, ...)); total }
print(hot("a"))
print(hot("a", a = 1))
