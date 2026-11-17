# Text becomes a number digit by digit in extended precision, so some decimal texts read one
# unit in the last place away from the nearest double. Literals and `as.numeric` agree.
show <- function(x) tryCatch(x, warning = function(w) paste("W:", conditionMessage(w)), error = function(e) paste("E:", conditionMessage(e)))
print(sprintf("%a", c(0.5127050331204, 9.8045974e-6, 1e23, 8.41e21, 0.1, 1e-5, 123456789012345678901234567890,
    0.000001234567890123456789, 1e22, 5e-324, 2.2250738585072014e-308, 2.2250738585072011e-308,
    0x1.fffffffffffffp1023, 0x1.00000000000008p0, 0x1.0000000000000801p0, 1e-320, 123456789e-330)))
print(sprintf("%a", as.numeric(c("0.5127050331204", "9.8045974e-6", "1e23", "0.000001234567890123456789",
    "123456789012345678901234567890", "1e-320", "123456789e-330", paste0("0.", strrep("0", 330), "123456789")))))
print(sprintf("%a", eval(parse(text = "c(0.5127050331204, 9.8045974e-6, 4.35, 0.3, 1e-310)"))))

# Range edges.
for (text in c("1e9999", "1e-9999", "1e400", "1e-400", "4.9e-324", "2.4e-324", "1.7976931348623157e308",
    "1.7976931348623158e308", "1.7976931348623159e308", "17976931348623158e292")) print(show(as.numeric(text)))
print(1 / as.numeric("-0"))

# Hexadecimal text: digits, a fraction, and a binary exponent whose power of two is a double.
for (text in c("0x1p-1074", "0x1p-1022", "0x1p-1030", "0x1.8", "0x.8p1", "0x1.8.8", "0X1P3", "0x", "0xg", "0x1p",
    "0x1p1024", "0x1p1023")) print(show(as.numeric(text)))

# What may surround the number, and what is not a number.
for (text in c("1 ", " 1", " ", "  ", "", "1.", ".5", "1e+", "1E5", "+-1", "iNfInItY", "infinit", "-nAn", "\t1\n",
    "1,5", "1d5", "TRUE", "1L", "1i", "NA", "1 ", " 1", "1​")) print(show(as.numeric(text)))
print(show(as.numeric("1\xff")))
print(show(as.numeric("\xff")))
print(show(as.integer("1\xff")))
print(show(as.complex("1\xff")))
latin1 <- "1\xa0"
Encoding(latin1) <- "latin1"
print(show(as.numeric(latin1)))

# Integers and complex numbers read through the same routine.
for (text in c("0x10", "1e3", " 12 ", "1.9", "-1.9", "NA", "3000000000", "-2147483648")) print(show(as.integer(text)))
for (text in c("1", "2i", "1+2i", "1-2i", "NA", "1+", "1+2", "0x10+1e1i", " 1+1i ", "Inf-Infi")) print(show(as.complex(text)))

# `type.convert` and `scan` accept `NA` silently and can refuse a loss of accuracy.
print(show(type.convert(c("9007199254740993", "1"), as.is = TRUE, numerals = "warn.loss")))
print(show(type.convert(c("9007199254740993", "1"), as.is = TRUE, numerals = "no.loss")))
print(show(type.convert("0x20000000000001", as.is = TRUE, numerals = "no.loss")))
print(show(type.convert(c("NA", "1.5"), as.is = TRUE)))
print(show(scan(text = "1,5 0x1p3 NA 1e", dec = ",", quiet = TRUE)))
print(show(scan(text = "1 2 x", quiet = TRUE)))

# Callers that coerce a string argument.
x <- c(a = "1", b = "NA", c = NA)
print(show(as.numeric(x)))
print(suppressWarnings(as.integer(x)))
print(show(mode(x) <- "numeric"))
print(show(storage.mode(x) <- "integer"))
print(show(as.double(factor("NA"))))
print(show(sum("1")))
print(show(seq_len("3")))
print(show(numeric("2")))
print(show(1:3 == "NA"))
print(show(vapply("NA", as.numeric, 0)))
print(show(matrix(1:4, "2")))
print(show(as.numeric(list("NA", "1"))))
print(show(as.vector(list("NA"), "numeric")))
print(show(is.na(as.numeric("NA_real_"))))
