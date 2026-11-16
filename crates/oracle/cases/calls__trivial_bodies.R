# Closures whose body is one variable or one constant, called often enough for a site to plan.
identity1 <- function(x) x
nothing <- function() NULL
seven <- function() 7L
label <- function() "label"
vector_constant <- function() c
global_value <- 10L
reads_global <- function() global_value
first_of <- function(x, ...) x
with_default <- function(x = stop("default forced")) x
frame_of <- function(e = environment()) e
call_of <- function(x = sys.call()) x
loop <- function(n) {
  total <- 0L
  local_value <- 3L
  for (i in seq_len(n)) {
    total <- total + identity1(i) + seven() + reads_global() + first_of(local_value, i, stop("never forced"))
    if (!is.null(nothing())) stop("not NULL")
  }
  list(total, label(), ls(frame_of()), call_of(), identity1(local_value + 1L))
}
str(loop(25L))
global_value <- 100L
str(loop(3L)[[1L]])
# The same calls from top-level code, which reads its variables by name.
total <- 0L
for (i in 1:25) total <- total + identity1(i) + seven() + reads_global()
print(total)
# Laziness and errors: the argument runs when the body reads it, and a missing argument or an
# unbound variable names the call.
for (i in 1:12) {
  out <- tryCatch(if (i > 10L) identity1() else identity1(i), error = function(e) conditionMessage(e))
}
print(out)
for (i in 1:12) out <- tryCatch(with_default(), error = function(e) c(conditionMessage(e), deparse(conditionCall(e))))
print(out)
unbound_body <- function() no_such_variable
for (i in 1:12) out <- tryCatch(unbound_body(), error = function(e) c(conditionMessage(e), deparse(conditionCall(e))))
print(out)
order <- character(0)
note <- function(tag) { order <<- c(order, tag); tag }
for (i in 1:12) pair <- c(first_of(note("a"), note("unused")), identity1(note("b")))
print(c(pair, length(order), unique(order)))
# Visibility follows the forced argument, and a constant is visible.
quiet <- function(n) { for (i in seq_len(n)) value <- withVisible(identity1(invisible(i))); value }
str(quiet(12L))
str(withVisible(seven()))
identity1(invisible(5))
# The value a constant body answers is not the body itself.
for (i in 1:12) { value <- seven(); value[2L] <- i }
print(seven())
print(body(seven))
copy <- vector_constant()
print(identical(copy, c))
# Bodies that are objects: a list, an expression vector, and a body replaced while a site has a
# plan. An environment is no body.
object_body <- function() NULL
print(tryCatch(body(object_body) <- globalenv(), error = function(e) conditionMessage(e)))
expression_body <- as.function(list(expression(1 + 2, x)))
for (i in 1:12) value <- expression_body()
str(value)
list_body <- as.function(list(list(1, "a")))
for (i in 1:12) value <- list_body()
str(value)
changing <- function() 1L
results <- integer(0)
for (i in 1:30) {
  if (i == 15L) body(changing) <- 2L
  if (i == 25L) body(changing) <- quote(global_value)
  results <- c(results, changing())
}
print(results)
# A promise as the variable, an active binding, `T`, and the variables only the evaluator runs.
makeActiveBinding("active", function() paste("read", global_value), environment())
reads_active <- function() active
for (i in 1:12) last <- reads_active()
print(last)
truth <- function() T
for (i in 1:12) last <- truth()
print(last)
second <- function(...) ..2
for (i in 1:12) last <- tryCatch(if (i > 10L) second(i) else second(i, i * 2L), error = function(e) conditionMessage(e))
print(last)
delayed <- function() { delayedAssign("later", note("forced")); function() later }
reader <- delayed()
for (i in 1:12) last <- reader()
print(c(last, sum(order == "forced")))
# Methods and replacement functions with such bodies.
format.plain <- function(x, ...) "plain"
print.plain <- function(x, ...) x
as_plain <- function(x) structure(x, class = "plain")
for (i in 1:12) text <- format(as_plain(i))
print(text)
visible <- withVisible(print(as_plain(1L)))
str(unclass(visible$value))
`untouched<-` <- function(x, value) x
vector <- 1:3
for (i in 1:12) untouched(vector) <- i
print(vector)
# `sys.function`, `match.call` and `environment` of a frame that ran no instruction but one.
function_of <- function(f = sys.function()) f
matched <- function(x, y = match.call()) y
for (i in 1:12) { shown <- deparse(function_of()); call <- matched(i + 1L) }
print(shown)
print(call)

# `function` refuses a body that is itself a function, as `as.function` does.
for (body in list(sum, quote, function() 1)) {
  print(tryCatch(eval(call("function", NULL, body)), error = conditionMessage))
}
print(eval(call("function", NULL, 1i))())
