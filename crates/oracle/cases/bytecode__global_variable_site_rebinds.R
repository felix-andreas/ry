# One `GetVar` site reads a free variable across attaches that mask each other, detaches, a
# global definition that masks them, reassignment and removal, so the site's cached cell must
# follow every change.
read_level <- function() level + 0
attach(list(level = 10), name = "lower")
for (i in 1:3) print(read_level())
attach(list(level = 20), name = "upper")
for (i in 1:2) print(read_level())
detach("upper")
print(read_level())
level <- 1
print(read_level())
level <- 2
print(read_level())
rm(level)
print(read_level())
detach("lower")
print(tryCatch(read_level(), error = conditionMessage))
level <- 3
print(read_level())
