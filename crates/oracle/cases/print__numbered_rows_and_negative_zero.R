# Numbered row labels take the width of one past the last row, so nine rows reserve two digits.
print(matrix(1:8, 8))
print(matrix(1:9, 9))
print(matrix(1:99, 99)[1:2, , drop = FALSE])
print(matrix(numeric(0), 0, 2))
print(matrix(1:10, 10, dimnames = list(NULL, "a")))
# Negative zero prints as zero, in fixed and in scientific notation.
print(c(-0, 1.5))
print(c(-0, 1e-300))
print(matrix(c(-0, 1e-300), 2))
