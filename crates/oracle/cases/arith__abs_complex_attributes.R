# abs() of a complex vector keeps its attributes, like Mod().
m <- matrix(complex(real = 1:3, imaginary = 1), 3, 1)
v <- complex(real = 4:6, imaginary = 2)
print(dim(v / m)); print(dim(abs(m))); print(dim(abs(v / m))); print(dim(2i * v / m))
r <- matrix(1:3 + 0, 3, 1); print(dim(v / r)); print(dim(abs(v/r))); print(dim(Mod(m))); print(dim(Arg(m)))
print(abs(c(a = 3+4i, b = 1i)))
