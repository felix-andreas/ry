show <- function(e) {
    if (is.call(e)) {
        head <- deparse(e[[1]])
        args <- vapply(as.list(e)[-1], show, "")
        if (head == "(") return(paste0("P[", args, "]"))
        paste0(head, "{", paste(args, collapse = ","), "}")
    } else deparse(e)
}
exprs <- c("x^2","x^n","n^x","exp(2*x)","log(x)","log(x, 2)","cos(x)","sin(x)","tan(x)","cosh(x)","sinh(x)","tanh(x)","sqrt(x)","pnorm(x)","dnorm(x)","asin(x)","acos(x)","atan(x)","gamma(x)","lgamma(x)","digamma(x)","trigamma(x)","psigamma(x)","psigamma(x, 2)","log1p(x)","expm1(x)","log2(x)","log10(x)","cospi(x)","sinpi(x)","tanpi(x)","abs(x)","-x","x-y","-x*y","x/y","y/x","(x+1)^3","x*y*x","1/x","2/x^2","-(x^2)","x^-1","exp(-x^2/2)","factorial(x)","lfactorial(x)","+x","(x)","a*x+b","x^y",
  "x^(a+b)", "x^(a-b)", "x^(a*b)", "x^(a/b)", "x^(a^b)", "x^(-a)", "(x+y)^z", "(x-y)^z", "(x*y)^z", "(x/y)^z", "(x^y)^z", "(-x)^z", "(x+y)^(a+b)", "x^(x*a)", "(x*a)^x", "(x*a)^(x*b)",
  "(x+y)*(x-y)", "x*y*z", "(x+1)/(x-1)", "x/(y*z)", "-x^2", "exp(x)/(1+exp(x))", "a/(1+b*exp(-c*x))", "sin(x)*cos(x)/x", "x/0", "0^x", "TRUE*x", "2L*x", "x^2L", "psigamma(2*x, k)", "sin(x)^2 + cos(x)*exp(x)")
for (e in exprs) {
    r <- tryCatch(D(parse(text = e)[[1]], "x"), error = function(err) conditionMessage(err))
    cat(e, " => ", show(r), "\n")
}
print(D(expression(x^2), "x"))
print(D(quote(y), "x"))
print(deriv(~ x^2, "x"))
print(deriv(y ~ sin(cos(x) * y), c("x","y"), func = TRUE))
print(deriv(quote(exp(-x^2/2)*a), c("x", "a"), hessian=TRUE))
f <- deriv(~ a*exp(-b*t), c("a","b"), function(a, b, t) {})
print(f)
print(f(2, 3, 1:2))
print(deriv3(~ x*y, c("x","y")))
g <- deriv(~ (x - m)^2 / (2 * s^2) + log(s), c("m", "s"), function.arg = TRUE)
print(g)
print(g(1:3, 2, 0.5))
print(deriv(~ sin(x)^2 + cos(x)^2, "x", hessian = TRUE))
