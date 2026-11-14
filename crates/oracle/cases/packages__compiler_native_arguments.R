# ry's compiler::compile returns its expression and cmpfun compiles a closure, but the compiler's
# options, a srcref and a bad env still reach GNU R's behavior: notes, forced arguments, errors.
compiled <- compiler::cmpfun(function(x) x * 2)
print(compiled(21))
invisible(compiler::compile(quote(a + b)))
invisible(compiler::compile(quote(f(1)), env = new.env()))
f <- compiler::cmpfun(function() undefined_fn(), options = list(suppressAll = FALSE))
print(is.function(f))
e <- compiler::compile(quote(undefined_fn()), options = list(suppressAll = FALSE))
print(tryCatch(compiler::cmpfun(function(x) x, options = stop("options forced")),
               error = conditionMessage))
print(tryCatch(compiler::compile(quote(1), env = 1), error = conditionMessage))
print(compiler::cmpfun(sum)(1:3))
traced <- character()
g <- compiler::compile({traced <- c(traced, "e"); quote(1)},
                       env = {traced <- c(traced, "env"); globalenv()})
print(traced)
# parent.env takes an environment, never a search position as as.environment does.
print(tryCatch(parent.env(1), error = function(e) conditionCall(e)))
print(tryCatch(parent.env(NULL), error = conditionMessage))
