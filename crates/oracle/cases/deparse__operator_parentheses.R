# Which operands deparse in parentheses. A unary operand on the right reads back the same bare
# (`a + !b`, `-!a`), and one that ends the left operand of a tighter operator does not.
unary <- function(op, x) as.call(list(as.name(op), x))
binary <- function(op, x, y) as.call(list(as.name(op), x, y))
show <- function(expression) cat(deparse(expression), "|", deparse(expression, control = "all"), "\n")

parsed <- list(quote(-!a), quote(!!a), quote(-!a + 1), quote((-!a)^2), quote(~-!a), quote(-!(b^TRUE)^1), quote(!-a),
    quote(- -a), quote(-(-a)), quote(!a + b), quote((!a) + b), quote(-a^2), quote((-a)^2), quote(a^-b), quote(a^!b),
    quote(a * -b), quote(a - -b), quote(a + !b), quote(!a == b), quote((!a) == b), quote(-a:b), quote((-a):b),
    quote(a:-b), quote(!a %in% b), quote(-a %in% b), quote(~a + b), quote((~a) + b), quote(a ~ -b), quote(-~a),
    quote(!~a), quote(?a), quote(-?a), quote(+a), quote(+-a), quote(-+a), quote(!a$b), quote(-a$b), quote((-a)$b),
    quote(-a[1]), quote((-a)[1]), quote(!a[[1]]), quote(-f(x)), quote(-(a + b)), quote(!(a && b)), quote(!a && b),
    quote(-a * b), quote(-(a * b)), quote(a && !b), quote(a || -b), quote(if (!a) -b), quote(function(x) -!x),
    quote(-if (a) b), quote(!function(x) x), quote(a <- -b), quote(a <- !b), quote(-a <- b), quote(a ? -b),
    quote(-a@b), quote(-a::b), quote(-2^-2), quote(-1), quote(- 1L), quote(!TRUE), quote(-"a"), quote(-NULL),
    quote(-(1)), quote(-{a}), quote(-a %% b), quote(-a %/% b), quote(a / -b / c), quote(a^b^-c), quote((a^b)^c),
    quote(a^(b^c)), quote(-a |> f()), quote(!a |> f()), quote(\(x) -x))
for (expression in parsed) show(expression)

# Calls built by hand, which no source text gives.
built <- list(unary("-", unary("!", quote(a))), unary("!", binary("+", quote(a), quote(b))),
    unary("-", binary("+", quote(a), quote(b))), binary("^", unary("-", quote(a)), 2), binary("^", unary("!", quote(a)), 2),
    binary("*", unary("!", quote(a)), quote(b)), binary("+", unary("!", quote(a)), 1), binary("+", 1, unary("!", quote(a))),
    unary("-", binary("^", quote(a), 2)), unary("!", binary("==", quote(a), quote(b))),
    binary("==", unary("!", quote(a)), quote(b)), unary("-", binary(":", quote(a), quote(b))),
    binary(":", unary("-", quote(a)), quote(b)), unary("~", unary("-", unary("!", quote(a)))), unary("-", unary("~", quote(a))),
    binary("$", unary("-", quote(a)), quote(b)), binary("[", unary("-", quote(a)), 1),
    unary("-", binary("<-", quote(a), quote(b))), binary("&&", unary("!", quote(a)), quote(b)),
    unary("!", binary("&&", quote(a), quote(b))), unary("+", unary("+", quote(a))), unary("-", -1), unary("-", -1L),
    unary("-", 1i), unary("-", NA), binary("^", -1, 2), binary("^", 2, -1), binary("-", quote(a), -1),
    binary("*", -1, quote(a)), unary("!", unary("-", binary("^", quote(b), TRUE))),
    binary("^", unary("-", unary("!", binary("^", quote(b), TRUE))), 1), unary("-", unary("(", unary("!", quote(a)))))
for (expression in built) show(expression)
f <- function(a, b) { -!a; !!a; -!a + 1; (-!a)^2; ~-!a; -!(b^TRUE)^1; !a + b }
print(body(f))

# Every operator over every kind of operand, on each side and inside the left operand of
# another operator.
definition <- quote(function(x) x)
condition <- quote(if (a) b)
operands <- list(definition, condition, quote(while (a) b), quote(repeat b), quote(for (i in a) b), unary("-", quote(a)),
    unary("!", quote(a)), unary("~", quote(a)), unary("+", condition), unary("!", definition),
    unary("-", unary("!", condition)), 1i, 1+2i, binary("<-", quote(a), quote(b)), binary("=", quote(a), quote(b)),
    binary("$", quote(a), quote(b)), binary("[", quote(a), 1), binary("%in%", quote(a), quote(b)),
    binary("==", quote(a), quote(b)), binary("::", quote(a), quote(b)), binary("?", quote(a), quote(b)),
    unary("?", quote(a)), binary("|>", quote(a), quote(f())), unary("(", quote(a)), unary("{", quote(a)), quote(f(x)))
for (op in c("+", "*", "^", "==", "&&", "<-", "=", "$", "@", "[", "[[", ":", "%in%", "~", "/", "%%", "::", "?", "<<-", "|")) {
    for (x in operands) for (y in list(quote(z), unary("-", quote(z)), condition)) {
        cat(deparse(binary(op, x, y)), "|", deparse(binary(op, y, x)), "|", deparse(binary("+", binary(op, y, x), 1)), "|",
            deparse(binary("^", binary(op, binary("*", y, x), 2), 3)), "\n")
    }
}
for (op in c("-", "+", "!", "~", "?", "*", "%%", "%in%", "(")) {
    for (x in operands) {
        cat(deparse(unary(op, x)), "|", deparse(binary("^", unary(op, x), 2)), "|",
            deparse(binary("+", binary("*", 2, unary(op, x)), 3)), "\n")
    }
}

# `$` and `@` are operators only before a name or a string; any other member is a call.
members <- list("b c", c("x", "y"), NA_character_, character(0), "if", 1, "", "b", quote(`b c`), quote(`if`), TRUE, NULL,
    "é", "é x", c("x y", "z"))
for (member in members) for (op in c("$", "@")) {
    cat(deparse(binary(op, quote(a), member)), "|", deparse(binary("+", binary(op, binary("+", quote(a), 1), member), 1)), "\n")
}
cat(deparse(as.call(list(as.name("$"), quote(a), quote(b), quote(c)))), deparse(as.call(list(as.name("$"), quote(a)))),
    deparse(as.call(list(as.name("$")))), "\n")
