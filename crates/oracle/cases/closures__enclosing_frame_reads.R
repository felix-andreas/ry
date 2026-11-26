# A closure made in a function reads and calls the variables of the frames that enclose it. Each
# closure runs several times, so a read is covered on its first run and on the runs after it.
cat("-- closures over one shared frame\n")
counter <- function() {
    count <- 0L
    step <- 1L
    add <- function() {
        count <<- count + step
        count
    }
    get <- function() count
    set_step <- function(value) step <<- value
    list(add = add, get = get, set_step = set_step)
}
object <- counter()
for (i in 1:4) print(object$add())
object$set_step(10L)
for (i in 1:3) print(c(object$add(), object$get()))
other <- counter()
for (i in 1:3) print(c(other$add(), object$get()))

cat("-- a variable removed from the enclosing frame and bound again\n")
value <- "global"
holder <- function() {
    value <- "local"
    function() value
}
read <- holder()
for (i in 1:3) print(read())
rm("value", envir = environment(read))
for (i in 1:3) print(read())
assign("value", "bound again", envir = environment(read))
for (i in 1:3) print(read())
value <- "global again"
rm("value", envir = environment(read))
for (i in 1:3) print(read())

cat("-- a nearer frame binds the name after the farther one was read\n")
outer_maker <- function() {
    shared <- "outer"
    middle <- function() {
        function() shared
    }
    middle()
}
inner <- outer_maker()
for (i in 1:3) print(inner())
assign("shared", "middle", envir = environment(inner))
for (i in 1:3) print(inner())
rm("shared", envir = environment(inner))
for (i in 1:3) print(inner())
assign("shared", "outer, replaced", envir = parent.env(environment(inner)))
for (i in 1:3) print(inner())

cat("-- a frame of many variables\n")
global_value <- "from the global environment"
wide <- function() {
    a1 <- 1; a2 <- 2; a3 <- 3; a4 <- 4; a5 <- 5; a6 <- 6; a7 <- 7; a8 <- 8; a9 <- 9; a10 <- 10
    b1 <- 11; b2 <- 12; b3 <- 13; b4 <- 14; b5 <- 15; b6 <- 16; b7 <- 17; b8 <- 18; b9 <- 19
    c1 <- 21; c2 <- 22; c3 <- 23; c4 <- 24; c5 <- 25; c6 <- 26; c7 <- 27; c8 <- 28; c9 <- 29
    d1 <- 31; d2 <- 32; d3 <- 33; d4 <- 34; d5 <- 35; d6 <- 36; d7 <- 37; d8 <- 38; d9 <- 39
    function(which) {
        switch(which, a1 + d9, b5 + c5, global_value, length(letters), a10 + a1 + b9 + c1 + d1)
    }
}
pick <- wide()
for (round in 1:3) for (which in 1:5) print(pick(which))
assign("global_value", "from the frame", envir = environment(pick))
assign("letters", c("x", "y"), envir = environment(pick))
for (which in 3:4) for (round in 1:2) print(pick(which))

cat("-- one function body over frames laid out differently\n")
laid_out <- function(first) {
    if (first) {
        x <- "bound first"
        padding <- 0
    } else {
        padding <- 0
        more <- 1
        x <- "bound last"
    }
    function() x
}
first <- laid_out(TRUE)
last <- laid_out(FALSE)
for (i in 1:3) print(c(first(), last()))
for (i in 1:3) print(c(last(), first()))

cat("-- a promise and an active binding in the enclosing frame\n")
lazy <- function(x) function() x
reader <- lazy({
    cat("forced\n")
    5L
})
for (i in 1:3) print(reader())
ticking <- function() {
    reads <- 0L
    makeActiveBinding("ticket", function() {
        reads <<- reads + 1L
        reads
    }, environment())
    function() ticket
}
ticket <- ticking()
for (i in 1:3) print(ticket())
never <- lazy(stop("no value"))
for (i in 1:3) print(tryCatch(never(), error = conditionMessage))
absent <- function(x) function() x
missing_reader <- absent()
for (i in 1:3) print(tryCatch(missing_reader(), error = conditionMessage))

cat("-- a function lookup passes a variable that is no function\n")
shadowed <- function() {
    c <- 1:3
    paste <- "not a function"
    function() paste(c(c, 4L), collapse = "")
}
joined <- shadowed()
for (i in 1:3) print(joined())
swapped <- function() {
    helper <- function() "one"
    list(call = function() helper(), swap = function() helper <<- function() "two",
         hide = function() helper <<- "not a function")
}
helper <- function() "the global helper"
object <- swapped()
for (i in 1:3) print(object$call())
object$swap()
for (i in 1:3) print(object$call())
object$hide()
for (i in 1:3) print(object$call())

cat("-- a callback reads the variables of a frame that is running\n")
running <- function(n) {
    total <- 0L
    for (i in seq_len(n)) {
        total <- total + i
        print(vapply(1:2, function(j) total + j, 0L))
    }
    bump <- function() total <<- total + 100L
    bump()
    total <- total + 1L
    sapply(1:2, function(j) total + j)
}
for (i in 1:3) print(running(3L))
nested <- function(depth) {
    label <- paste0("depth ", depth)
    describe <- function() {
        deeper <- function() label
        deeper()
    }
    if (depth > 0L) c(describe(), nested(depth - 1L)) else describe()
}
for (i in 1:3) print(nested(2L))

cat("-- a closure made in a `local` block inside a function\n")
blocked <- function() {
    x <- "the function's"
    local({
        function() x
    })
}
reads_block <- blocked()
for (i in 1:3) print(reads_block())
assign("x", "the block's", envir = environment(reads_block))
for (i in 1:3) print(reads_block())

cat("-- two dozen variables read beside globals and base functions\n")
global_a <- 1L
global_b <- 2L
global_c <- 3L
global_d <- 4L
m3 <- "the global m3"
medium <- function() {
    m1 <- 1L; m2 <- 2L; m3 <- 3L; m4 <- 4L; m5 <- 5L; m6 <- 6L; m7 <- 7L; m8 <- 8L
    m9 <- 9L; m10 <- 10L; m11 <- 11L; m12 <- 12L; m13 <- 13L; m14 <- 14L; m15 <- 15L; m16 <- 16L
    m17 <- 17L; m18 <- 18L; m19 <- 19L; m20 <- 20L; m21 <- 21L; m22 <- 22L; m23 <- 23L; m24 <- 24L
    function(i) {
        numbers <- c(m1 + m24, global_a + global_b + global_c + global_d, length(letters),
                     nchar("abc"), max(m5, i), min(m7, i), abs(-i), sum(m2, m9), rev(c(m4, i)),
                     nrow(matrix(m10, 2L, 2L)), ncol(matrix(m11, 1L, 3L)), as.integer(m12 / 2))
        flags <- c(is.null(m6), identical(m8, 8L), is.numeric(m13), is.character(m14),
                   any(m15 > i), all(m16 > i), isTRUE(m17 == 17L), nzchar(toupper("a")))
        list(numbers, flags, m3)
    }
}
reads_medium <- medium()
for (i in 1:3) str(reads_medium(i))
assign("global_c", 30L, envir = environment(reads_medium))
assign("length", function(x) -1L, envir = environment(reads_medium))
for (i in 1:2) str(reads_medium(i))
rm("m3", "global_c", "length", envir = environment(reads_medium))
for (i in 1:2) str(reads_medium(i))
other_medium <- medium()
assign("abs", function(x) 0L, envir = environment(other_medium))
for (i in 1:2) str(list(reads_medium(i)[[1]], other_medium(i)[[1]]))
