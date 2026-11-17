# A number with an `L` suffix that is not an integer, or that spells a decimal point, warns. The
# lexer words the warning by whether it saw a point and no exponent, where a hexadecimal
# literal's exponent starts with `p`.
for (s in c(".5e-3L", "0x1.1p1L", "0x1.8p1L", "1.0L", "1.5L", "1e3L", "1.0e1L", "1.5e1L", "1.25e1L", "1e-1L", "0x10L", "0x1p-1L", "0X1P2L", "1.L", "100000000000L", "1.0e10L", "2147483647L", "2147483648L", "1E0L", "1.0E0L")) {
  r <- withCallingHandlers(eval(parse(text = s)), warning = function(w) { cat(s, "W:", conditionMessage(w), "\n"); invokeRestart("muffleWarning") })
  cat(s, "->", typeof(r), format(r), "\n")
}
