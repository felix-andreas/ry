# numeric

Read this before you change the numeric kernels. Cross-cutting traps live in `.agents/memory/MEMORY.md`.

## Traps

- **Numeric ports match GNU R's last bit, so port the routine, not the formula**: keep the 4.6.2 evaluation order (`x**3` is `(x*x)*x`, sums left to right) and find a difference with a grid sweep before reading code. A residual few-ulp gap usually sits in `crates/numeric/src/linpack.rs`.
- **Translate a C `do { ... } while (a > b)` that can meet NaN as `if !(a > b) { break }`**, never `if a <= b { break }`, which spins forever on NaN (`rmath.rs`).
