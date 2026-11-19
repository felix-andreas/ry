# Decimal text reads to the double GNU R's long-double reader gives, which for about one text
# in five thousand is one unit in the last place from the correctly rounded double.
set.seed(42)
digits <- c(sprintf("%.3f", runif(400, -1000, 1000)), sprintf("%.13f", runif(400)),
            sprintf("%.15g", rnorm(400) * 10^sample(-20:20, 400, TRUE)),
            sprintf("%d", sample.int(1e9, 100)), sprintf("%.0f", runif(50, 0, 2^63)),
            "0.5127050331204", "9.8045974e-6", "0.1", "1e23", "123456789012345678901",
            "4503599627370497.5", "9007199254740993", "0.000000000000000000001", "-0", "1e-22",
            "1.7976931348623157e308", "2.2250738585072014e-308", "12345678901234567890e-19")
values <- as.numeric(digits)
cat(sprintf("%s %a", digits, values), sep = "\n")
print(identical(values, utils::type.convert(digits, as.is = TRUE)))
print(identical(values, scan(text = digits, quiet = TRUE)))
