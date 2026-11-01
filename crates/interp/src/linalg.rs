//! Linear algebra: the matrix products, base R's LAPACK internals (`La_solve`, `La_svd`,
//! `La_rs`, ...), `backsolve`, and the LINPACK routines base registers for `.Fortran`
//! (`dqrdc2` and the `dqrsl` family, `dtrco`).
//!
//! Results must match GNU R bit for bit, so the numerical work runs in the same libraries GNU R
//! links: the system's BLAS and LAPACK, opened at runtime (the workspace builds without them),
//! called with the same arguments, the same workspace queries and the same Fortran calling
//! convention (every argument by reference, a hidden length after the arguments for each
//! character argument). `dqrdc2` is R's own variant of LINPACK's QR, which no system library
//! ships, so it is written here from its description: Householder QR whose column pivoting only
//! moves a column to the end once its remaining norm falls below `tol` times its original norm.

use {
    crate::{
        builtins::{Arg, BuiltinDef, Call, Generic, builtin, internal},
        coerce::coerce_vector,
        conditions,
        unwind::{R, error},
        values::{
            self, alloc, complexes, complexes_mut, dims, get_attr, ints, ints_mut, length, nil,
            reals, reals_mut, symbols,
        },
    },
    heap::object::{Rcomplex, Sexp, SexpType, gp},
    std::{
        ffi::{CStr, c_char, c_void},
        sync::OnceLock,
    },
};

pub const BUILTINS: &[BuiltinDef] = &[
    builtin("%*%", 2, |call| unsafe {
        // R 4.3's `%*%` dispatches S4 methods, positionally, when either operand is an S4
        // object, and never S3 ones.
        if call
            .args
            .iter()
            .take(2)
            .any(|arg| arg.value.has_gp(gp::S4_OBJECT))
        {
            let positional: Vec<Arg> = call
                .args
                .iter()
                .map(|arg| Arg {
                    tag: nil(),
                    value: arg.value,
                })
                .collect();
            if let Some(value) =
                crate::dispatch::s4_dispatch(call.op, call.call, &positional, call.env)?
            {
                return Ok(value);
            }
        }
        matrix_product(call, Product::Matrix)
    })
    .generic(Generic::MatrixOps),
    // R 4.4 made `crossprod` and `tcrossprod` primitives in the `matrixOps` group; before that
    // base R defined them as closures over the internals. Both exist so that either base loads.
    builtin("crossprod", -1, |call| unsafe {
        matrix_product(call, Product::Cross)
    })
    .generic(Generic::MatrixOps),
    builtin("tcrossprod", -1, |call| unsafe {
        matrix_product(call, Product::TransposedCross)
    })
    .generic(Generic::MatrixOps),
    internal("crossprod", 2, |call| unsafe {
        matrix_product(call, Product::Cross)
    }),
    internal("tcrossprod", 2, |call| unsafe {
        matrix_product(call, Product::TransposedCross)
    }),
    internal("La_solve", 3, |call| unsafe {
        la_solve(call.arg(0), call.arg(1), call.arg(2))
    }),
    internal("La_solve_cmplx", 3, |call| unsafe {
        la_solve_complex(call.arg(0), call.arg(1), call.arg(2))
    }),
    internal("La_chol", 3, |call| unsafe {
        la_chol(call.arg(0), call.arg(1), call.arg(2))
    }),
    internal("La_chol2inv", 2, |call| unsafe {
        la_chol2inv(call.arg(0), call.arg(1))
    }),
    internal("La_qr", 1, |call| unsafe { la_qr(call.arg(0)) }),
    internal("La_qr_cmplx", 1, |call| unsafe {
        la_qr_complex(call.arg(0))
    }),
    internal("qr_coef_real", 2, |call| unsafe {
        qr_coef_real(call.arg(0), call.arg(1))
    }),
    internal("qr_coef_cmplx", 2, |call| unsafe {
        qr_coef_complex(call.arg(0), call.arg(1))
    }),
    internal("qr_qy_real", 3, |call| unsafe {
        qr_qy_real(call.arg(0), call.arg(1), call.arg(2))
    }),
    internal("qr_qy_cmplx", 3, |call| unsafe {
        qr_qy_complex(call.arg(0), call.arg(1), call.arg(2))
    }),
    internal("La_svd", 5, |call| unsafe { la_svd(call) }),
    internal("La_svd_cmplx", 5, |call| unsafe { la_svd_complex(call) }),
    internal("La_rs", 2, |call| unsafe {
        la_rs(call.arg(0), call.arg(1))
    }),
    internal("La_rs_cmplx", 2, |call| unsafe {
        la_rs_complex(call.arg(0), call.arg(1))
    }),
    internal("La_rg", 2, |call| unsafe {
        la_rg(call.arg(0), call.arg(1))
    }),
    internal("La_rg_cmplx", 2, |call| unsafe {
        la_rg_complex(call.arg(0), call.arg(1))
    }),
    internal("det_ge_real", 2, |call| unsafe {
        det_ge_real(call.arg(0), call.arg(1))
    }),
    internal("La_dgecon", 2, |call| unsafe {
        la_dgecon(call.arg(0), call.arg(1))
    }),
    internal("La_dtrcon", 2, |call| unsafe {
        la_dtrcon(call.arg(0), call.arg(1), c"U")
    }),
    // R 4.4's `rcond(triangular = TRUE, uplo = "L")` and `.kappa_tri(uplo =)`.
    internal("La_dtrcon3", 3, |call| unsafe {
        la_dtrcon(call.arg(0), call.arg(1), uplo(call.arg(2))?)
    }),
    internal("La_zgecon", 2, |call| unsafe {
        la_zgecon(call.arg(0), call.arg(1))
    }),
    internal("La_ztrcon", 2, |call| unsafe {
        la_ztrcon(call.arg(0), call.arg(1), c"U")
    }),
    internal("La_ztrcon3", 3, |call| unsafe {
        la_ztrcon(call.arg(0), call.arg(1), uplo(call.arg(2))?)
    }),
    internal("La_dlange", 2, |call| unsafe {
        la_dlange(call.arg(0), call.arg(1))
    }),
    // R 4.4's `norm()` of a complex matrix.
    internal("La_zlange", 2, |call| unsafe {
        la_zlange(call.arg(0), call.arg(1))
    }),
    internal("backsolve", 5, |call| unsafe { backsolve(call) }),
    internal("La_version", 0, |_| unsafe { la_version() }),
    internal("La_library", 0, |_| unsafe { la_library() }),
];

// The matrix products.

#[derive(Clone, Copy, PartialEq, Eq)]
enum Product {
    /// `x %*% y`
    Matrix,
    /// `crossprod(x, y)`: `t(x) %*% y`
    Cross,
    /// `tcrossprod(x, y)`: `x %*% t(y)`
    TransposedCross,
}

/// `%*%`, `crossprod` and `tcrossprod`, with GNU R's default `matprod` policy: BLAS unless an
/// operand may hold a NaN or an infinity, where a plain loop keeps IEEE propagation that an
/// optimized BLAS may not. Reference BLAS sums in the same order as that loop, so the loop also
/// stands in for BLAS when the library is missing.
unsafe fn matrix_product(call: &Call<'_>, product: Product) -> R<Sexp> {
    let internal = crate::interp()
        .builtin(call.op.primitive_index())
        .is_some_and(|definition| definition.is_internal());
    let report = |message: &str| {
        if !internal {
            call.error(message)
        } else {
            error(message)
        }
    };
    let x = values::materialize(call.arg(0))?;
    let symmetric = product != Product::Matrix && call.arg(1) == nil();
    let y = if symmetric {
        x
    } else {
        values::materialize(call.arg(1))?
    };
    if !is_numeric_or_complex(x) || !is_numeric_or_complex(y) {
        return Err(report("requires numeric/complex matrix/vector arguments"));
    }
    let x_dims = matrix_dims(x);
    let y_dims = matrix_dims(y);
    let (x_length, y_length) = (length(x), length(y));
    let ((x_rows, x_cols), (y_rows, y_cols)) = match (x_dims, y_dims) {
        (Some(x_shape), Some(y_shape)) => (x_shape, y_shape),
        // A scalar `x` multiplies a row vector `y`; otherwise two vectors make an inner product
        // (or, for `tcrossprod`, an outer one).
        (None, None) => match product {
            Product::Matrix if x_length == 1 => ((1, 1), (1, y_length)),
            Product::Matrix => ((1, x_length), (y_length, 1)),
            Product::Cross if x_length == 1 => ((1, 1), (1, y_length)),
            _ => ((x_length, 1), (y_length, 1)),
        },
        (None, Some((y_rows, y_cols))) => {
            let x_shape = match product {
                Product::Matrix if x_length == y_rows => (1, y_rows),
                Product::Matrix if y_rows == 1 => (x_length, 1),
                Product::Cross if x_length == y_rows => (y_rows, 1),
                Product::TransposedCross if x_length == y_cols => (1, y_cols),
                Product::TransposedCross if y_cols == 1 => (x_length, 1),
                _ => (0, 0),
            };
            (x_shape, (y_rows, y_cols))
        }
        (Some((x_rows, x_cols)), None) => {
            let y_shape = match product {
                Product::Matrix if y_length == x_cols => (x_cols, 1),
                Product::Matrix if x_cols == 1 => (1, y_length),
                Product::Cross if y_length == x_rows => (x_rows, 1),
                Product::Cross if x_rows == 1 => (1, y_length),
                Product::TransposedCross if y_length == x_cols => (1, x_cols),
                Product::TransposedCross if x_cols == 1 => (y_length, 1),
                _ => (0, 0),
            };
            ((x_rows, x_cols), y_shape)
        }
    };
    let conformable = match product {
        Product::Matrix => x_cols == y_rows,
        Product::Cross => x_rows == y_rows,
        Product::TransposedCross => x_cols == y_cols,
    };
    if !conformable {
        return Err(report("non-conformable arguments"));
    }
    let mode = if x.kind() == SexpType::Complex || y.kind() == SexpType::Complex {
        SexpType::Complex
    } else {
        SexpType::Real
    };
    let x_values = coerce_vector(x, mode)?;
    let y_values = if symmetric {
        x_values
    } else {
        coerce_vector(y, mode)?
    };
    let shape = Shape {
        x_rows,
        x_cols,
        y_rows,
        y_cols,
    };
    let (result_rows, result_cols) = match product {
        Product::Matrix => (x_rows, y_cols),
        Product::Cross => (x_cols, y_cols),
        Product::TransposedCross => (x_rows, y_rows),
    };
    let result = alloc_matrix(mode, result_rows, result_cols)?;
    if mode == SexpType::Complex {
        complex_product(
            product,
            &shape,
            complexes(x_values),
            complexes(y_values),
            complexes_mut(result),
        );
    } else {
        real_product(
            product,
            symmetric,
            &shape,
            reals(x_values),
            reals(y_values),
            reals_mut(result),
        );
    }
    product_dimnames(
        product,
        x,
        y,
        x_dims.is_some(),
        y_dims.is_some(),
        &shape,
        result,
    )?;
    Ok(result)
}

struct Shape {
    x_rows: usize,
    x_cols: usize,
    y_rows: usize,
    y_cols: usize,
}

unsafe fn real_product(
    product: Product,
    symmetric: bool,
    shape: &Shape,
    x: &[f64],
    y: &[f64],
    z: &mut [f64],
) {
    let Shape {
        x_rows,
        x_cols,
        y_rows,
        y_cols,
    } = *shape;
    if x_rows == 0 || x_cols == 0 || y_rows == 0 || y_cols == 0 {
        z.fill(0.0);
        return;
    }
    let blas = blas()
        .ok()
        .filter(|_| !may_have_nan_or_inf(x) && !may_have_nan_or_inf(y));
    let (m, k, n) = (x_rows as i32, x_cols as i32, y_cols as i32);
    let (one, zero, unit) = (1.0f64, 0.0f64, 1i32);
    match (product, blas) {
        (Product::Matrix, Some(blas)) => {
            if y_cols == 1 {
                (blas.dgemv)(
                    c"N".as_ptr(),
                    &m,
                    &k,
                    &one,
                    x.as_ptr(),
                    &m,
                    y.as_ptr(),
                    &unit,
                    &zero,
                    z.as_mut_ptr(),
                    &unit,
                    1,
                );
            } else if x_rows == 1 {
                let y_rows = y_rows as i32;
                (blas.dgemv)(
                    c"T".as_ptr(),
                    &y_rows,
                    &n,
                    &one,
                    y.as_ptr(),
                    &y_rows,
                    x.as_ptr(),
                    &unit,
                    &zero,
                    z.as_mut_ptr(),
                    &unit,
                    1,
                );
            } else {
                (blas.dgemm)(
                    c"N".as_ptr(),
                    c"N".as_ptr(),
                    &m,
                    &n,
                    &k,
                    &one,
                    x.as_ptr(),
                    &m,
                    y.as_ptr(),
                    &k,
                    &zero,
                    z.as_mut_ptr(),
                    &m,
                    1,
                    1,
                );
            }
        }
        (Product::Matrix, None) => {
            for row in 0..x_rows {
                for col in 0..y_cols {
                    let mut sum = 0.0;
                    for inner in 0..x_cols {
                        sum += x[row + inner * x_rows] * y[inner + col * y_rows];
                    }
                    z[row + col * x_rows] = sum;
                }
            }
        }
        (Product::Cross, Some(blas)) if symmetric => {
            (blas.dsyrk)(
                c"U".as_ptr(),
                c"T".as_ptr(),
                &k,
                &m,
                &one,
                x.as_ptr(),
                &m,
                &zero,
                z.as_mut_ptr(),
                &k,
                1,
                1,
            );
            mirror_upper(z, x_cols);
        }
        (Product::Cross, Some(blas)) => {
            (blas.dgemm)(
                c"T".as_ptr(),
                c"N".as_ptr(),
                &k,
                &n,
                &m,
                &one,
                x.as_ptr(),
                &m,
                y.as_ptr(),
                &m,
                &zero,
                z.as_mut_ptr(),
                &k,
                1,
                1,
            );
        }
        (Product::Cross, None) => {
            for row in 0..x_cols {
                for col in 0..y_cols {
                    let mut sum = 0.0;
                    for inner in 0..x_rows {
                        sum += x[inner + row * x_rows] * y[inner + col * y_rows];
                    }
                    z[row + col * x_cols] = sum;
                }
            }
        }
        (Product::TransposedCross, Some(blas)) if symmetric => {
            (blas.dsyrk)(
                c"U".as_ptr(),
                c"N".as_ptr(),
                &m,
                &k,
                &one,
                x.as_ptr(),
                &m,
                &zero,
                z.as_mut_ptr(),
                &m,
                1,
                1,
            );
            mirror_upper(z, x_rows);
        }
        (Product::TransposedCross, Some(blas)) => {
            let y_rows = y_rows as i32;
            (blas.dgemm)(
                c"N".as_ptr(),
                c"T".as_ptr(),
                &m,
                &y_rows,
                &k,
                &one,
                x.as_ptr(),
                &m,
                y.as_ptr(),
                &y_rows,
                &zero,
                z.as_mut_ptr(),
                &m,
                1,
                1,
            );
        }
        (Product::TransposedCross, None) => {
            for row in 0..x_rows {
                for col in 0..y_rows {
                    let mut sum = 0.0;
                    for inner in 0..x_cols {
                        sum += x[row + inner * x_rows] * y[col + inner * y_rows];
                    }
                    z[row + col * x_rows] = sum;
                }
            }
        }
    }
}

unsafe fn complex_product(
    product: Product,
    shape: &Shape,
    x: &[Rcomplex],
    y: &[Rcomplex],
    z: &mut [Rcomplex],
) {
    let Shape {
        x_rows,
        x_cols,
        y_rows,
        y_cols,
    } = *shape;
    if x_rows == 0 || x_cols == 0 || y_rows == 0 || y_cols == 0 {
        z.fill(Rcomplex { r: 0.0, i: 0.0 });
        return;
    }
    let finite = |values: &[Rcomplex]| {
        values
            .iter()
            .all(|value| value.r.is_finite() && value.i.is_finite())
    };
    let blas = blas().ok().filter(|_| finite(x) && finite(y));
    let one = Rcomplex { r: 1.0, i: 0.0 };
    let zero = Rcomplex { r: 0.0, i: 0.0 };
    let (x_rows_int, y_rows_int) = (x_rows as i32, y_rows as i32);
    let (z_rows, z_cols, inner_count, transpose_x, transpose_y) = match product {
        Product::Matrix => (x_rows, y_cols, x_cols, false, false),
        Product::Cross => (x_cols, y_cols, x_rows, true, false),
        Product::TransposedCross => (x_rows, y_rows, x_cols, false, true),
    };
    if let Some(blas) = blas {
        let transa = if transpose_x { c"T" } else { c"N" };
        let transb = if transpose_y { c"T" } else { c"N" };
        let (m, n, k) = (z_rows as i32, z_cols as i32, inner_count as i32);
        (blas.zgemm)(
            transa.as_ptr(),
            transb.as_ptr(),
            &m,
            &n,
            &k,
            &one,
            x.as_ptr(),
            &x_rows_int,
            y.as_ptr(),
            &y_rows_int,
            &zero,
            z.as_mut_ptr(),
            &m,
            1,
            1,
        );
        return;
    }
    for row in 0..z_rows {
        for col in 0..z_cols {
            let mut sum = Rcomplex { r: 0.0, i: 0.0 };
            for inner in 0..inner_count {
                let left = if transpose_x {
                    x[inner + row * x_rows]
                } else {
                    x[row + inner * x_rows]
                };
                let right = if transpose_y {
                    y[col + inner * y_rows]
                } else {
                    y[inner + col * y_rows]
                };
                let term = c99_multiply(left, right);
                sum.r += term.r;
                sum.i += term.i;
            }
            z[row + col * z_rows] = sum;
        }
    }
}

/// The product C99 defines for `double complex` (Annex G), which GNU R's NaN-safe complex
/// products use: plain arithmetic, except that an infinite operand yields an infinite result
/// where the plain formula gives NaN.
fn c99_multiply(left: Rcomplex, right: Rcomplex) -> Rcomplex {
    let (mut a, mut b, mut c, mut d) = (left.r, left.i, right.r, right.i);
    let (ac, bd, ad, bc) = (a * c, b * d, a * d, b * c);
    let (real, imaginary) = (ac - bd, ad + bc);
    if !(real.is_nan() && imaginary.is_nan()) {
        return Rcomplex {
            r: real,
            i: imaginary,
        };
    }
    let box_infinite =
        |value: f64| (if value.is_infinite() { 1.0f64 } else { 0.0 }).copysign(value);
    let zero_nan = |value: f64| {
        if value.is_nan() {
            0.0f64.copysign(value)
        } else {
            value
        }
    };
    let mut recalculate = false;
    if a.is_infinite() || b.is_infinite() {
        a = box_infinite(a);
        b = box_infinite(b);
        c = zero_nan(c);
        d = zero_nan(d);
        recalculate = true;
    }
    if c.is_infinite() || d.is_infinite() {
        c = box_infinite(c);
        d = box_infinite(d);
        a = zero_nan(a);
        b = zero_nan(b);
        recalculate = true;
    }
    if !recalculate
        && (ac.is_infinite() || bd.is_infinite() || ad.is_infinite() || bc.is_infinite())
    {
        a = zero_nan(a);
        b = zero_nan(b);
        c = zero_nan(c);
        d = zero_nan(d);
        recalculate = true;
    }
    if recalculate {
        return Rcomplex {
            r: f64::INFINITY * (a * c - b * d),
            i: f64::INFINITY * (a * d + b * c),
        };
    }
    Rcomplex {
        r: real,
        i: imaginary,
    }
}

/// GNU R's cheap screen for NaN and infinity: it sums neighbours, so two huge finite values that
/// overflow also send the product down the loop, which gives the same result.
fn may_have_nan_or_inf(values: &[f64]) -> bool {
    let odd = values.len() % 2;
    if odd == 1 && !values[0].is_finite() {
        return true;
    }
    values[odd..]
        .as_chunks::<2>()
        .0
        .iter()
        .any(|pair| !(pair[0] + pair[1]).is_finite())
}

/// `dsyrk` fills the upper triangle only.
fn mirror_upper(z: &mut [f64], size: usize) {
    for col in 0..size {
        for row in col + 1..size {
            z[row + col * size] = z[col + row * size];
        }
    }
}

/// The result's dimnames, from the operands' as GNU R picks them: a vector operand contributes
/// no names, except that `%*%` keeps a row vector's or column vector's dimnames when it is used
/// as such.
unsafe fn product_dimnames(
    product: Product,
    x: Sexp,
    y: Sexp,
    x_is_matrix: bool,
    y_is_matrix: bool,
    shape: &Shape,
    result: Sexp,
) -> R<()> {
    let x_dimnames = get_attr(x, symbols().dimnames);
    let y_dimnames = get_attr(y, symbols().dimnames);
    if x_dimnames == nil() && y_dimnames == nil() {
        return Ok(());
    }
    let pick = |dimnames: Sexp, index: usize| -> (Sexp, Option<Sexp>) {
        let names = values::names(dimnames);
        let label = (names != nil()).then(|| names.element(index));
        (list_or_nil(dimnames, index), label)
    };
    let x_index = match product {
        Product::Matrix if x_is_matrix || shape.x_cols == 1 => Some(0),
        Product::Cross if x_is_matrix => Some(1),
        Product::TransposedCross if x_is_matrix => Some(0),
        _ => None,
    };
    let y_index = match product {
        Product::Matrix | Product::Cross if y_is_matrix => Some(1),
        Product::Matrix | Product::Cross if shape.y_rows == 1 => Some(0),
        Product::TransposedCross if y_is_matrix => Some(0),
        _ => None,
    };
    let (rows, row_label) = match x_index.filter(|_| x_dimnames != nil()) {
        Some(index) => pick(x_dimnames, index),
        None => (nil(), None),
    };
    let (cols, col_label) = match y_index.filter(|_| y_dimnames != nil()) {
        Some(index) => pick(y_dimnames, index),
        None => (nil(), None),
    };
    if rows == nil() && cols == nil() {
        return Ok(());
    }
    let dimnames = values::list_vector(&[rows, cols])?;
    if row_label.is_some() || col_label.is_some() {
        let labels = alloc(SexpType::String, 2)?;
        for (index, label) in [row_label, col_label].into_iter().enumerate() {
            values::set_elt(labels, index, label.unwrap_or_else(values::blank_string));
        }
        values::set_attr(dimnames, symbols().names, labels)?;
    }
    values::set_attr(result, symbols().dimnames, dimnames)
}

// Linear systems and decompositions.

/// `solve(a, b)` through `dgesv`, with the condition check `tol` asks for.
unsafe fn la_solve(a: Sexp, b: Sexp, tol: Sexp) -> R<Sexp> {
    let lapack = lapack()?;
    let tol = values::as_real_scalar(tol);
    if !matches!(
        a.kind(),
        SexpType::Real | SexpType::Integer | SexpType::Logical
    ) || matrix_dims(a).is_none()
    {
        return Err(error("'a' must be a numeric matrix"));
    }
    let n = square_for_solve(a)?;
    let (result, columns) = solve_right_hand_side(a, b, n, SexpType::Real)?;
    let b_values = coerce_vector(b, SexpType::Real)?;
    reals_mut(result).copy_from_slice(&reals(b_values)[..n * columns]);
    let a_was_real = a.kind() == SexpType::Real;
    let mut factors = reals(coerce_vector(a, SexpType::Real)?).to_vec();
    let mut pivots = vec![0i32; n];
    let (order, rhs) = (n as i32, columns as i32);
    let mut info = 0;
    (lapack.dgesv)(
        &order,
        &rhs,
        factors.as_mut_ptr(),
        &order,
        pivots.as_mut_ptr(),
        reals_mut(result).as_mut_ptr(),
        &order,
        &mut info,
    );
    if info < 0 {
        return Err(error(format!(
            "argument {} of Lapack routine dgesv had invalid value",
            -info
        )));
    }
    if info > 0 {
        return Err(error(format!(
            "Lapack routine dgesv: system is exactly singular: U[{info},{info}] = 0"
        )));
    }
    if tol > 0.0 {
        // GNU R takes the norm of its working copy, which for an integer or logical `a` is the
        // copy `dgesv` has just factored.
        let norm_source = if a_was_real {
            reals(a).to_vec()
        } else {
            factors.clone()
        };
        let anorm = (lapack.dlange)(
            c"1".as_ptr(),
            &order,
            &order,
            norm_source.as_ptr(),
            &order,
            std::ptr::null_mut(),
            1,
        );
        let mut work = vec![0.0; 4 * n];
        let mut rcond = 0.0;
        (lapack.dgecon)(
            c"1".as_ptr(),
            &order,
            factors.as_ptr(),
            &order,
            &anorm,
            &mut rcond,
            work.as_mut_ptr(),
            pivots.as_mut_ptr(),
            &mut info,
            1,
        );
        if rcond < tol {
            return Err(error(format!(
                "system is computationally singular: reciprocal condition number = {}",
                format_g(rcond)
            )));
        }
    }
    Ok(result)
}

unsafe fn la_solve_complex(a: Sexp, b: Sexp, tol: Sexp) -> R<Sexp> {
    let lapack = lapack()?;
    let tol = values::as_real_scalar(tol);
    if matrix_dims(a).is_none() {
        return Err(error("'a' must be a complex matrix"));
    }
    let n = square_for_solve(a)?;
    let (result, columns) = solve_right_hand_side(a, b, n, SexpType::Complex)?;
    let b_values = coerce_vector(b, SexpType::Complex)?;
    complexes_mut(result).copy_from_slice(&complexes(b_values)[..n * columns]);
    let a_was_complex = a.kind() == SexpType::Complex;
    let mut factors = complexes(coerce_vector(a, SexpType::Complex)?).to_vec();
    let mut pivots = vec![0i32; n];
    let (order, rhs) = (n as i32, columns as i32);
    let mut info = 0;
    (lapack.zgesv)(
        &order,
        &rhs,
        factors.as_mut_ptr(),
        &order,
        pivots.as_mut_ptr(),
        complexes_mut(result).as_mut_ptr(),
        &order,
        &mut info,
    );
    if info < 0 {
        return Err(error(format!(
            "argument {} of Lapack routine zgesv had invalid value",
            -info
        )));
    }
    if info > 0 {
        return Err(error("Lapack routine zgesv: system is exactly singular"));
    }
    if tol > 0.0 {
        // As in `la_solve`, a coerced `a` is normed after factoring.
        let norm_source = if a_was_complex {
            complexes(a).to_vec()
        } else {
            factors.clone()
        };
        let mut real_work = vec![0.0; 2 * n];
        let anorm = (lapack.zlange)(
            c"1".as_ptr(),
            &order,
            &order,
            norm_source.as_ptr(),
            &order,
            real_work.as_mut_ptr(),
            1,
        );
        let mut work = vec![Rcomplex::default(); 2 * n];
        let mut rcond = 0.0;
        (lapack.zgecon)(
            c"1".as_ptr(),
            &order,
            factors.as_ptr(),
            &order,
            &anorm,
            &mut rcond,
            work.as_mut_ptr(),
            real_work.as_mut_ptr(),
            &mut info,
            1,
        );
        if rcond < tol {
            return Err(error(format!(
                "system is computationally singular: reciprocal condition number = {}",
                format_g(rcond)
            )));
        }
    }
    Ok(result)
}

unsafe fn square_for_solve(a: Sexp) -> R<usize> {
    let (rows, cols) = matrix_dims(a).unwrap_or((0, 0));
    if rows == 0 {
        return Err(error("'a' is 0-diml"));
    }
    if cols != rows {
        return Err(error(format!("'a' ({rows} x {cols}) must be square")));
    }
    Ok(rows)
}

/// The result `solve` fills: shaped like `b`, named by `a`'s column names and `b`'s.
unsafe fn solve_right_hand_side(a: Sexp, b: Sexp, n: usize, kind: SexpType) -> R<(Sexp, usize)> {
    let a_dimnames = get_attr(a, symbols().dimnames);
    if let Some((b_rows, columns)) = matrix_dims(b) {
        if columns == 0 {
            return Err(error("no right-hand side in 'b'"));
        }
        if b_rows != n {
            return Err(error(format!(
                "'b' ({b_rows} x {columns}) must be compatible with 'a' ({n} x {n})"
            )));
        }
        let result = alloc_matrix(kind, n, columns)?;
        let b_dimnames = get_attr(b, symbols().dimnames);
        if a_dimnames != nil() || b_dimnames != nil() {
            let rows = list_or_nil(a_dimnames, 1);
            let cols = list_or_nil(b_dimnames, 1);
            if rows != nil() || cols != nil() {
                let dimnames = values::list_vector(&[rows, cols])?;
                values::set_attr(result, symbols().dimnames, dimnames)?;
            }
        }
        return Ok((result, columns));
    }
    if kind == SexpType::Real
        && !matches!(
            b.kind(),
            SexpType::Logical | SexpType::Integer | SexpType::Real
        )
    {
        return Err(error("'b' must be a numeric matrix"));
    }
    if length(b) != n {
        return Err(error(format!(
            "'b' ({} x 1) must be compatible with 'a' ({n} x {n})",
            length(b)
        )));
    }
    let result = alloc(kind, n)?;
    let names = list_or_nil(a_dimnames, 1);
    if names != nil() {
        values::set_attr(result, symbols().names, names)?;
    }
    Ok((result, 1))
}

/// `chol(x, pivot, tol)`: `dpotrf`, or `dpstrf` with pivoting.
unsafe fn la_chol(a: Sexp, pivot: Sexp, tol: Sexp) -> R<Sexp> {
    let lapack = lapack()?;
    let Some((rows, cols)) = matrix_dims(a) else {
        return Err(error("'a' must be a numeric matrix"));
    };
    let result = fresh_copy(a, SexpType::Real)?;
    if rows != cols {
        return Err(error("'a' must be a square matrix"));
    }
    if rows == 0 {
        return Err(error("'a' must have dims > 0"));
    }
    let n = rows;
    let values_out = reals_mut(result);
    for col in 0..n {
        for row in col + 1..n {
            values_out[row + n * col] = 0.0;
        }
    }
    let pivoting = values::as_integer_scalar(pivot);
    if pivoting != 0 && pivoting != 1 {
        return Err(error("invalid 'pivot' value"));
    }
    let order = n as i32;
    let mut info = 0;
    if pivoting == 0 {
        (lapack.dpotrf)(
            c"U".as_ptr(),
            &order,
            values_out.as_mut_ptr(),
            &order,
            &mut info,
            1,
        );
        if info > 0 {
            return Err(error(format!(
                "the leading minor of order {info} is not positive"
            )));
        }
        if info < 0 {
            return Err(error(format!(
                "argument {} of Lapack routine dpotrf had invalid value",
                -info
            )));
        }
        return Ok(result);
    }
    let tol = values::as_real_scalar(tol);
    let mut work = vec![0.0; 2 * n];
    let pivots = alloc(SexpType::Integer, n)?;
    let mut rank = 0;
    (lapack.dpstrf)(
        c"U".as_ptr(),
        &order,
        values_out.as_mut_ptr(),
        &order,
        ints_mut(pivots).as_mut_ptr(),
        &mut rank,
        &tol,
        work.as_mut_ptr(),
        &mut info,
        1,
    );
    if info > 0 {
        conditions::warning("the matrix is either rank-deficient or not positive definite")?;
    } else if info < 0 {
        return Err(error(format!(
            "argument {} of Lapack routine dpstrf had invalid value",
            -info
        )));
    }
    values::set_attr(result, values::install("pivot")?, pivots)?;
    values::set_attr(
        result,
        values::install("rank")?,
        values::scalar_integer(rank)?,
    )?;
    permute_column_names(result, ints(pivots))?;
    Ok(result)
}

/// `chol2inv(x, size)`: the inverse from the Cholesky factor in `x`'s leading `size` columns.
unsafe fn la_chol2inv(a: Sexp, size: Sexp) -> R<Sexp> {
    let lapack = lapack()?;
    let size = values::as_integer_scalar(size);
    if values::is_na_int(size) || size < 1 {
        return Err(error("'size' argument must be a positive integer"));
    }
    let size = size as usize;
    let (rows, cols) = match matrix_dims(a) {
        Some(shape) => shape,
        None if a.kind() == SexpType::Real => (length(a), 1),
        None => return Err(error("'a' must be a numeric matrix")),
    };
    if size > cols {
        return Err(error(format!("'size' cannot exceed ncol(x) = {cols}")));
    }
    if size > rows {
        return Err(error(format!("'size' cannot exceed nrow(x) = {rows}")));
    }
    let source = coerce_vector(a, SexpType::Real)?;
    let result = alloc_matrix(SexpType::Real, size, size)?;
    let (input, output) = (reals(source), reals_mut(result));
    output.fill(0.0);
    for col in 0..size {
        for row in 0..=col {
            output[row + col * size] = input[row + col * rows];
        }
    }
    let order = size as i32;
    let mut info = 0;
    (lapack.dpotri)(
        c"U".as_ptr(),
        &order,
        output.as_mut_ptr(),
        &order,
        &mut info,
        1,
    );
    if info > 0 {
        return Err(error(format!(
            "element ({info}, {info}) is zero, so the inverse cannot be computed"
        )));
    }
    if info < 0 {
        return Err(error(format!(
            "argument {} of Lapack routine dpotri had invalid value",
            -info
        )));
    }
    for col in 0..size {
        for row in col + 1..size {
            output[row + col * size] = output[col + row * size];
        }
    }
    Ok(result)
}

/// `qr(x, LAPACK = TRUE)`: `dgeqp3`, whose pivoting is full column pivoting.
unsafe fn la_qr(a: Sexp) -> R<Sexp> {
    let lapack = lapack()?;
    let Some((rows, cols)) = matrix_dims(a).filter(|_| values::is_numeric_type(a.kind())) else {
        return Err(error("'a' must be a numeric matrix"));
    };
    let result = fresh_copy(a, SexpType::Real)?;
    let pivots = alloc(SexpType::Integer, cols)?;
    ints_mut(pivots).fill(0);
    let tau = alloc(SexpType::Real, rows.min(cols))?;
    let (m, n) = (rows as i32, cols as i32);
    let mut info = 0;
    let mut query = 0.0;
    let lwork = -1;
    (lapack.dgeqp3)(
        &m,
        &n,
        reals_mut(result).as_mut_ptr(),
        &m,
        ints_mut(pivots).as_mut_ptr(),
        reals_mut(tau).as_mut_ptr(),
        &mut query,
        &lwork,
        &mut info,
    );
    lapack_status(info < 0, info, "dgeqp3")?;
    let lwork = query as i32;
    let mut work = vec![0.0; lwork.max(1) as usize];
    (lapack.dgeqp3)(
        &m,
        &n,
        reals_mut(result).as_mut_ptr(),
        &m,
        ints_mut(pivots).as_mut_ptr(),
        reals_mut(tau).as_mut_ptr(),
        work.as_mut_ptr(),
        &lwork,
        &mut info,
    );
    lapack_status(info < 0, info, "dgeqp3")?;
    permute_column_names(result, ints(pivots))?;
    qr_list(result, rows.min(cols), tau, pivots)
}

unsafe fn la_qr_complex(a: Sexp) -> R<Sexp> {
    let lapack = lapack()?;
    let Some((rows, cols)) = matrix_dims(a) else {
        return Err(error("'a' must be a complex matrix"));
    };
    let result = fresh_copy(a, SexpType::Complex)?;
    let pivots = alloc(SexpType::Integer, cols)?;
    ints_mut(pivots).fill(0);
    let tau = alloc(SexpType::Complex, rows.min(cols))?;
    let mut real_work = vec![0.0; 2 * cols];
    let (m, n) = (rows as i32, cols as i32);
    let mut info = 0;
    let mut query = Rcomplex::default();
    let lwork = -1;
    (lapack.zgeqp3)(
        &m,
        &n,
        complexes_mut(result).as_mut_ptr(),
        &m,
        ints_mut(pivots).as_mut_ptr(),
        complexes_mut(tau).as_mut_ptr(),
        &mut query,
        &lwork,
        real_work.as_mut_ptr(),
        &mut info,
    );
    lapack_status(info < 0, info, "zgeqp3")?;
    let lwork = query.r as i32;
    let mut work = vec![Rcomplex::default(); lwork.max(1) as usize];
    (lapack.zgeqp3)(
        &m,
        &n,
        complexes_mut(result).as_mut_ptr(),
        &m,
        ints_mut(pivots).as_mut_ptr(),
        complexes_mut(tau).as_mut_ptr(),
        work.as_mut_ptr(),
        &lwork,
        real_work.as_mut_ptr(),
        &mut info,
    );
    lapack_status(info < 0, info, "zgeqp3")?;
    permute_column_names(result, ints(pivots))?;
    qr_list(result, rows.min(cols), tau, pivots)
}

unsafe fn qr_list(qr: Sexp, rank: usize, tau: Sexp, pivots: Sexp) -> R<Sexp> {
    values::named_list(&[
        ("qr", qr),
        ("rank", values::scalar_integer(rank as i32)?),
        ("qraux", tau),
        ("pivot", pivots),
    ])
}

/// The pieces of a LAPACK `qr` object the coefficient and `Q` products read: the compact
/// factor, its row count, and the Householder scalars.
struct LapackQr {
    qr: Sexp,
    rows: usize,
    tau: Sexp,
}

unsafe fn lapack_qr(qr_object: Sexp, kind: SexpType, right_rows: usize) -> R<LapackQr> {
    if qr_object.kind() != SexpType::List || length(qr_object) < 3 {
        return Err(error("invalid 'qr' argument"));
    }
    let qr = qr_object.element(0);
    let tau = qr_object.element(2);
    let Some((rows, cols)) = matrix_dims(qr).filter(|_| qr.kind() == kind && tau.kind() == kind)
    else {
        return Err(error("invalid 'qr' argument"));
    };
    if rows != right_rows {
        return Err(error(format!(
            "right-hand side should have {rows} not {right_rows} rows"
        )));
    }
    if length(tau) > rows.min(cols) {
        return Err(error("invalid 'qr' argument"));
    }
    Ok(LapackQr { qr, rows, tau })
}

/// `qr.coef` for a LAPACK decomposition: `Q'y` by `dormqr`, then the triangular solve.
unsafe fn qr_coef_real(qr_object: Sexp, b: Sexp) -> R<Sexp> {
    let lapack = lapack()?;
    let Some((rows, rhs_count)) = matrix_dims(b) else {
        return Err(error("'b' must be a numeric matrix"));
    };
    let result = fresh_copy(b, SexpType::Real)?;
    let factor = lapack_qr(qr_object, SexpType::Real, rows)?;
    let k = length(factor.tau) as i32;
    let (n, rhs) = (factor.rows as i32, rhs_count as i32);
    multiply_by_q_real(lapack, &factor, result, c"T", n, rhs, k)?;
    let mut info = 0;
    (lapack.dtrtrs)(
        c"U".as_ptr(),
        c"N".as_ptr(),
        c"N".as_ptr(),
        &k,
        &rhs,
        reals(factor.qr).as_ptr(),
        &n,
        reals_mut(result).as_mut_ptr(),
        &n,
        &mut info,
        1,
        1,
        1,
    );
    lapack_status(info != 0, info, "dtrtrs")?;
    Ok(result)
}

unsafe fn qr_coef_complex(qr_object: Sexp, b: Sexp) -> R<Sexp> {
    let lapack = lapack()?;
    let Some((rows, rhs_count)) = matrix_dims(b) else {
        return Err(error("'b' must be a complex matrix"));
    };
    let result = fresh_copy(b, SexpType::Complex)?;
    let factor = lapack_qr(qr_object, SexpType::Complex, rows)?;
    let k = length(factor.tau) as i32;
    let (n, rhs) = (factor.rows as i32, rhs_count as i32);
    multiply_by_q_complex(lapack, &factor, result, c"C", n, rhs, k)?;
    let mut info = 0;
    (lapack.ztrtrs)(
        c"U".as_ptr(),
        c"N".as_ptr(),
        c"N".as_ptr(),
        &k,
        &rhs,
        complexes(factor.qr).as_ptr(),
        &n,
        complexes_mut(result).as_mut_ptr(),
        &n,
        &mut info,
        1,
        1,
        1,
    );
    lapack_status(info != 0, info, "ztrtrs")?;
    Ok(result)
}

/// `qr.qy` and `qr.qty` for a LAPACK decomposition.
unsafe fn qr_qy_real(qr_object: Sexp, b: Sexp, transpose: Sexp) -> R<Sexp> {
    let lapack = lapack()?;
    let Some((rows, rhs_count)) = matrix_dims(b) else {
        return Err(error("'b' must be a numeric matrix"));
    };
    let transpose = transpose_flag(transpose)?;
    let result = fresh_copy(b, SexpType::Real)?;
    let factor = lapack_qr(qr_object, SexpType::Real, rows)?;
    let k = length(factor.tau) as i32;
    let trans = if transpose { c"T" } else { c"N" };
    multiply_by_q_real(
        lapack,
        &factor,
        result,
        trans,
        factor.rows as i32,
        rhs_count as i32,
        k,
    )?;
    Ok(result)
}

unsafe fn qr_qy_complex(qr_object: Sexp, b: Sexp, transpose: Sexp) -> R<Sexp> {
    let lapack = lapack()?;
    // Unlike `qr_coef_cmplx`, GNU R does not coerce a real `b` here.
    let Some((rows, rhs_count)) = matrix_dims(b).filter(|_| b.kind() == SexpType::Complex) else {
        return Err(error("'b' must be a complex matrix"));
    };
    let transpose = transpose_flag(transpose)?;
    let result = values::duplicate(b)?;
    let factor = lapack_qr(qr_object, SexpType::Complex, rows)?;
    let k = length(factor.tau) as i32;
    let trans = if transpose { c"C" } else { c"N" };
    multiply_by_q_complex(
        lapack,
        &factor,
        result,
        trans,
        factor.rows as i32,
        rhs_count as i32,
        k,
    )?;
    Ok(result)
}

unsafe fn transpose_flag(transpose: Sexp) -> R<bool> {
    match values::as_logical_scalar(transpose) {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err(error("invalid 'trans' argument")),
    }
}

unsafe fn multiply_by_q_real(
    lapack: &Lapack,
    factor: &LapackQr,
    target: Sexp,
    trans: &CStr,
    n: i32,
    rhs: i32,
    k: i32,
) -> R<()> {
    let mut info = 0;
    let mut query = 0.0;
    let lwork = -1;
    (lapack.dormqr)(
        c"L".as_ptr(),
        trans.as_ptr(),
        &n,
        &rhs,
        &k,
        reals(factor.qr).as_ptr(),
        &n,
        reals(factor.tau).as_ptr(),
        reals_mut(target).as_mut_ptr(),
        &n,
        &mut query,
        &lwork,
        &mut info,
        1,
        1,
    );
    lapack_status(info != 0, info, "dormqr")?;
    let lwork = query as i32;
    let mut work = vec![0.0; lwork.max(1) as usize];
    (lapack.dormqr)(
        c"L".as_ptr(),
        trans.as_ptr(),
        &n,
        &rhs,
        &k,
        reals(factor.qr).as_ptr(),
        &n,
        reals(factor.tau).as_ptr(),
        reals_mut(target).as_mut_ptr(),
        &n,
        work.as_mut_ptr(),
        &lwork,
        &mut info,
        1,
        1,
    );
    lapack_status(info != 0, info, "dormqr")
}

unsafe fn multiply_by_q_complex(
    lapack: &Lapack,
    factor: &LapackQr,
    target: Sexp,
    trans: &CStr,
    n: i32,
    rhs: i32,
    k: i32,
) -> R<()> {
    let mut info = 0;
    let mut query = Rcomplex::default();
    let lwork = -1;
    (lapack.zunmqr)(
        c"L".as_ptr(),
        trans.as_ptr(),
        &n,
        &rhs,
        &k,
        complexes(factor.qr).as_ptr(),
        &n,
        complexes(factor.tau).as_ptr(),
        complexes_mut(target).as_mut_ptr(),
        &n,
        &mut query,
        &lwork,
        &mut info,
        1,
        1,
    );
    lapack_status(info != 0, info, "zunmqr")?;
    let lwork = query.r as i32;
    let mut work = vec![Rcomplex::default(); lwork.max(1) as usize];
    (lapack.zunmqr)(
        c"L".as_ptr(),
        trans.as_ptr(),
        &n,
        &rhs,
        &k,
        complexes(factor.qr).as_ptr(),
        &n,
        complexes(factor.tau).as_ptr(),
        complexes_mut(target).as_mut_ptr(),
        &n,
        work.as_mut_ptr(),
        &lwork,
        &mut info,
        1,
        1,
    );
    lapack_status(info != 0, info, "zunmqr")
}

/// The arguments `La.svd` passes: `jobu`, `x`, and the `d`, `u` and `vt` it preallocated, whose
/// shapes tell `dgesdd` how many singular vectors to compute.
struct SvdArgs {
    job: String,
    rows: i32,
    cols: i32,
    d: Sexp,
    u: Sexp,
    vt: Sexp,
    u_rows: i32,
    vt_rows: i32,
}

unsafe fn svd_args(call: &Call<'_>, kind: SexpType) -> R<SvdArgs> {
    let job = call.arg(0);
    if job.kind() != SexpType::String || length(job) < 1 {
        return Err(error("'jobu' must be a character string"));
    }
    let job = values::char_str(job.element(0)).into_owned();
    let Some((rows, cols)) = matrix_dims(call.arg(1)) else {
        return Err(error("'x' must be a numeric or complex matrix"));
    };
    let d = fresh_copy(call.arg(2), SexpType::Real)?;
    let u = fresh_copy(call.arg(3), kind)?;
    let vt = fresh_copy(call.arg(4), kind)?;
    let (Some((u_rows, u_cols)), Some((vt_rows, vt_cols))) = (matrix_dims(u), matrix_dims(vt))
    else {
        return Err(error("'u' and 'vt' must be matrices"));
    };
    let smaller = rows.min(cols);
    let (u_need, vt_need) = match job.as_str() {
        "N" => ((1, 1), (1, 1)),
        "A" => ((rows, rows), (cols, cols)),
        "S" => ((rows, smaller), (smaller, cols)),
        _ => return Err(error(format!("invalid 'jobu' value '{job}'"))),
    };
    if length(d) < smaller
        || u_rows < u_need.0
        || u_cols < u_need.1
        || vt_rows < vt_need.0
        || vt_cols < vt_need.1
    {
        return Err(error(
            "the preallocated singular value storage is too small",
        ));
    }
    Ok(SvdArgs {
        job,
        rows: rows as i32,
        cols: cols as i32,
        d,
        u,
        vt,
        u_rows: u_rows as i32,
        vt_rows: vt_rows as i32,
    })
}

/// `La.svd(x)`: the divide-and-conquer SVD, `dgesdd`.
unsafe fn la_svd(call: &Call<'_>) -> R<Sexp> {
    let lapack = lapack()?;
    let args = svd_args(call, SexpType::Real)?;
    let mut x = reals(coerce_vector(call.arg(1), SexpType::Real)?).to_vec();
    let smaller = args.rows.min(args.cols) as usize;
    let mut integer_work = vec![0i32; 8 * smaller];
    let job =
        std::ffi::CString::new(args.job.as_str()).map_err(|_| error("invalid 'jobu' value"))?;
    let mut info = 0;
    let mut query = 0.0;
    let lwork = -1;
    (lapack.dgesdd)(
        job.as_ptr(),
        &args.rows,
        &args.cols,
        x.as_mut_ptr(),
        &args.rows,
        reals_mut(args.d).as_mut_ptr(),
        reals_mut(args.u).as_mut_ptr(),
        &args.u_rows,
        reals_mut(args.vt).as_mut_ptr(),
        &args.vt_rows,
        &mut query,
        &lwork,
        integer_work.as_mut_ptr(),
        &mut info,
        1,
    );
    lapack_status(info != 0, info, "dgesdd")?;
    let lwork = query as i32;
    let mut work = vec![0.0; lwork.max(1) as usize];
    (lapack.dgesdd)(
        job.as_ptr(),
        &args.rows,
        &args.cols,
        x.as_mut_ptr(),
        &args.rows,
        reals_mut(args.d).as_mut_ptr(),
        reals_mut(args.u).as_mut_ptr(),
        &args.u_rows,
        reals_mut(args.vt).as_mut_ptr(),
        &args.vt_rows,
        work.as_mut_ptr(),
        &lwork,
        integer_work.as_mut_ptr(),
        &mut info,
        1,
    );
    lapack_status(info != 0, info, "dgesdd")?;
    values::named_list(&[("d", args.d), ("u", args.u), ("vt", args.vt)])
}

unsafe fn la_svd_complex(call: &Call<'_>) -> R<Sexp> {
    let lapack = lapack()?;
    let args = svd_args(call, SexpType::Complex)?;
    let mut x = complexes(coerce_vector(call.arg(1), SexpType::Complex)?).to_vec();
    let smaller = args.rows.min(args.cols) as usize;
    let larger = args.rows.max(args.cols) as usize;
    let real_work_size = if args.job == "N" {
        7 * smaller
    } else {
        (5 * smaller * smaller + 5 * smaller)
            .max(2 * larger * smaller + 2 * smaller * smaller + smaller)
    };
    let mut real_work = vec![0.0; real_work_size.max(1)];
    let mut integer_work = vec![0i32; 8 * smaller];
    let job =
        std::ffi::CString::new(args.job.as_str()).map_err(|_| error("invalid 'jobu' value"))?;
    let mut info = 0;
    let mut query = Rcomplex::default();
    let lwork = -1;
    (lapack.zgesdd)(
        job.as_ptr(),
        &args.rows,
        &args.cols,
        x.as_mut_ptr(),
        &args.rows,
        reals_mut(args.d).as_mut_ptr(),
        complexes_mut(args.u).as_mut_ptr(),
        &args.u_rows,
        complexes_mut(args.vt).as_mut_ptr(),
        &args.vt_rows,
        &mut query,
        &lwork,
        real_work.as_mut_ptr(),
        integer_work.as_mut_ptr(),
        &mut info,
        1,
    );
    lapack_status(info != 0, info, "zgesdd")?;
    let lwork = query.r as i32;
    let mut work = vec![Rcomplex::default(); lwork.max(1) as usize];
    (lapack.zgesdd)(
        job.as_ptr(),
        &args.rows,
        &args.cols,
        x.as_mut_ptr(),
        &args.rows,
        reals_mut(args.d).as_mut_ptr(),
        complexes_mut(args.u).as_mut_ptr(),
        &args.u_rows,
        complexes_mut(args.vt).as_mut_ptr(),
        &args.vt_rows,
        work.as_mut_ptr(),
        &lwork,
        real_work.as_mut_ptr(),
        integer_work.as_mut_ptr(),
        &mut info,
        1,
    );
    lapack_status(info != 0, info, "zgesdd")?;
    values::named_list(&[("d", args.d), ("u", args.u), ("vt", args.vt)])
}

unsafe fn square_order(x: Sexp, message: &str) -> R<usize> {
    match matrix_dims(x) {
        Some((rows, cols)) if rows == cols => Ok(rows),
        _ => Err(error(message)),
    }
}

unsafe fn only_values_flag(only_values: Sexp) -> R<bool> {
    match values::as_logical_scalar(only_values) {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err(error("invalid 'only.values' argument")),
    }
}

/// `eigen(x, symmetric = TRUE)`: `dsyevr` on the lower triangle, eigenvalues ascending.
unsafe fn la_rs(x: Sexp, only_values: Sexp) -> R<Sexp> {
    let lapack = lapack()?;
    let n = square_order(x, "'x' must be a square numeric matrix")?;
    let only_values = only_values_flag(only_values)?;
    let mut matrix = reals(coerce_vector(x, SexpType::Real)?).to_vec();
    let eigenvalues = alloc(SexpType::Real, n)?;
    let vectors = if only_values {
        None
    } else {
        Some(alloc_matrix(SexpType::Real, n, n)?)
    };
    let vectors_pointer = vectors.map_or(std::ptr::null_mut(), |vectors| {
        reals_mut(vectors).as_mut_ptr()
    });
    let job = if only_values { c"N" } else { c"V" };
    let order = n as i32;
    let (lower_bound, upper_bound, abstol) = (0.0f64, 0.0f64, 0.0f64);
    let (lower_index, upper_index) = (0i32, 0i32);
    let mut found = 0;
    let mut support = vec![0i32; 2 * n.max(1)];
    let mut info = 0;
    let mut query = 0.0;
    let mut integer_query = 0i32;
    let (lwork, liwork) = (-1, -1);
    (lapack.dsyevr)(
        job.as_ptr(),
        c"A".as_ptr(),
        c"L".as_ptr(),
        &order,
        matrix.as_mut_ptr(),
        &order,
        &lower_bound,
        &upper_bound,
        &lower_index,
        &upper_index,
        &abstol,
        &mut found,
        reals_mut(eigenvalues).as_mut_ptr(),
        vectors_pointer,
        &order,
        support.as_mut_ptr(),
        &mut query,
        &lwork,
        &mut integer_query,
        &liwork,
        &mut info,
        1,
        1,
        1,
    );
    lapack_status(info != 0, info, "dsyevr")?;
    let (lwork, liwork) = (query as i32, integer_query);
    let mut work = vec![0.0; lwork.max(1) as usize];
    let mut integer_work = vec![0i32; liwork.max(1) as usize];
    (lapack.dsyevr)(
        job.as_ptr(),
        c"A".as_ptr(),
        c"L".as_ptr(),
        &order,
        matrix.as_mut_ptr(),
        &order,
        &lower_bound,
        &upper_bound,
        &lower_index,
        &upper_index,
        &abstol,
        &mut found,
        reals_mut(eigenvalues).as_mut_ptr(),
        vectors_pointer,
        &order,
        support.as_mut_ptr(),
        work.as_mut_ptr(),
        &lwork,
        integer_work.as_mut_ptr(),
        &liwork,
        &mut info,
        1,
        1,
        1,
    );
    lapack_status(info != 0, info, "dsyevr")?;
    match vectors {
        Some(vectors) => values::named_list(&[("values", eigenvalues), ("vectors", vectors)]),
        None => values::named_list(&[("values", eigenvalues)]),
    }
}

/// `eigen(x, symmetric = TRUE)` for a complex `x`: `zheev`.
unsafe fn la_rs_complex(x: Sexp, only_values: Sexp) -> R<Sexp> {
    let lapack = lapack()?;
    let n = square_order(x, "'x' must be a square complex matrix")?;
    let only_values = only_values_flag(only_values)?;
    let matrix = alloc_matrix(SexpType::Complex, n, n)?;
    complexes_mut(matrix).copy_from_slice(complexes(coerce_vector(x, SexpType::Complex)?));
    let eigenvalues = alloc(SexpType::Real, n)?;
    let job = if only_values { c"N" } else { c"V" };
    let order = n as i32;
    let mut real_work = vec![0.0; (3 * n).saturating_sub(2).max(1)];
    let mut info = 0;
    let mut query = Rcomplex::default();
    let lwork = -1;
    (lapack.zheev)(
        job.as_ptr(),
        c"L".as_ptr(),
        &order,
        complexes_mut(matrix).as_mut_ptr(),
        &order,
        reals_mut(eigenvalues).as_mut_ptr(),
        &mut query,
        &lwork,
        real_work.as_mut_ptr(),
        &mut info,
        1,
        1,
    );
    lapack_status(info != 0, info, "zheev")?;
    let lwork = query.r as i32;
    let mut work = vec![Rcomplex::default(); lwork.max(1) as usize];
    (lapack.zheev)(
        job.as_ptr(),
        c"L".as_ptr(),
        &order,
        complexes_mut(matrix).as_mut_ptr(),
        &order,
        reals_mut(eigenvalues).as_mut_ptr(),
        work.as_mut_ptr(),
        &lwork,
        real_work.as_mut_ptr(),
        &mut info,
        1,
        1,
    );
    lapack_status(info != 0, info, "zheev")?;
    if only_values {
        values::named_list(&[("values", eigenvalues)])
    } else {
        values::named_list(&[("values", eigenvalues), ("vectors", matrix)])
    }
}

/// `eigen(x, symmetric = FALSE)`: `dgeev`, right eigenvectors only. The values are complex as
/// soon as one has an imaginary part that is not negligible next to its real part.
unsafe fn la_rg(x: Sexp, only_values: Sexp) -> R<Sexp> {
    let lapack = lapack()?;
    let n = square_order(x, "'x' must be a square numeric matrix")?;
    let mut matrix = reals(coerce_vector(x, SexpType::Real)?).to_vec();
    let only_values = only_values_flag(only_values)?;
    let mut right = if only_values {
        Vec::new()
    } else {
        vec![0.0; n * n]
    };
    let right_pointer = if only_values {
        std::ptr::null_mut()
    } else {
        right.as_mut_ptr()
    };
    let job_right = if only_values { c"N" } else { c"V" };
    let mut real_parts = vec![0.0; n];
    let mut imaginary_parts = vec![0.0; n];
    let order = n as i32;
    let mut info = 0;
    let mut query = 0.0;
    let lwork = -1;
    (lapack.dgeev)(
        c"N".as_ptr(),
        job_right.as_ptr(),
        &order,
        matrix.as_mut_ptr(),
        &order,
        real_parts.as_mut_ptr(),
        imaginary_parts.as_mut_ptr(),
        std::ptr::null_mut(),
        &order,
        right_pointer,
        &order,
        &mut query,
        &lwork,
        &mut info,
        1,
        1,
    );
    lapack_status(info != 0, info, "dgeev")?;
    let lwork = query as i32;
    let mut work = vec![0.0; lwork.max(1) as usize];
    (lapack.dgeev)(
        c"N".as_ptr(),
        job_right.as_ptr(),
        &order,
        matrix.as_mut_ptr(),
        &order,
        real_parts.as_mut_ptr(),
        imaginary_parts.as_mut_ptr(),
        std::ptr::null_mut(),
        &order,
        right_pointer,
        &order,
        work.as_mut_ptr(),
        &lwork,
        &mut info,
        1,
        1,
    );
    lapack_status(info != 0, info, "dgeev")?;
    let complex_values = real_parts
        .iter()
        .zip(&imaginary_parts)
        .any(|(real, imaginary)| imaginary.abs() > 10.0 * f64::EPSILON * real.abs());
    let (eigenvalues, vectors) = if complex_values {
        let eigenvalues = alloc(SexpType::Complex, n)?;
        for (index, value) in complexes_mut(eigenvalues).iter_mut().enumerate() {
            *value = Rcomplex {
                r: real_parts[index],
                i: imaginary_parts[index],
            };
        }
        let vectors = if only_values {
            nil()
        } else {
            unscramble(&imaginary_parts, n, &right)?
        };
        (eigenvalues, vectors)
    } else {
        let eigenvalues = values::real_vector(&real_parts)?;
        let vectors = if only_values {
            nil()
        } else {
            let vectors = alloc_matrix(SexpType::Real, n, n)?;
            reals_mut(vectors).copy_from_slice(&right);
            vectors
        };
        (eigenvalues, vectors)
    };
    values::named_list(&[("values", eigenvalues), ("vectors", vectors)])
}

/// `dgeev` stores a complex conjugate pair of eigenvectors as two real columns, the real and
/// the imaginary part of the first.
unsafe fn unscramble(imaginary_parts: &[f64], n: usize, packed: &[f64]) -> R<Sexp> {
    let result = alloc_matrix(SexpType::Complex, n, n)?;
    let output = complexes_mut(result);
    let mut col = 0;
    while col < n {
        if imaginary_parts[col] != 0.0 && col + 1 < n {
            for row in 0..n {
                let real = packed[row + col * n];
                let imaginary = packed[row + (col + 1) * n];
                output[row + col * n] = Rcomplex {
                    r: real,
                    i: imaginary,
                };
                output[row + (col + 1) * n] = Rcomplex {
                    r: real,
                    i: -imaginary,
                };
            }
            col += 2;
        } else {
            for row in 0..n {
                output[row + col * n] = Rcomplex {
                    r: packed[row + col * n],
                    i: 0.0,
                };
            }
            col += 1;
        }
    }
    Ok(result)
}

unsafe fn la_rg_complex(x: Sexp, only_values: Sexp) -> R<Sexp> {
    let lapack = lapack()?;
    let n = square_order(x, "'x' must be a square numeric matrix")?;
    let mut matrix = complexes(coerce_vector(x, SexpType::Complex)?).to_vec();
    let only_values = only_values_flag(only_values)?;
    let vectors = if only_values {
        None
    } else {
        Some(alloc_matrix(SexpType::Complex, n, n)?)
    };
    let right_pointer = vectors.map_or(std::ptr::null_mut(), |vectors| {
        complexes_mut(vectors).as_mut_ptr()
    });
    let job_right = if only_values { c"N" } else { c"V" };
    let eigenvalues = alloc(SexpType::Complex, n)?;
    let mut real_work = vec![0.0; 2 * n];
    let order = n as i32;
    let mut info = 0;
    let mut query = Rcomplex::default();
    let lwork = -1;
    (lapack.zgeev)(
        c"N".as_ptr(),
        job_right.as_ptr(),
        &order,
        matrix.as_mut_ptr(),
        &order,
        complexes_mut(eigenvalues).as_mut_ptr(),
        std::ptr::null_mut(),
        &order,
        right_pointer,
        &order,
        &mut query,
        &lwork,
        real_work.as_mut_ptr(),
        &mut info,
        1,
        1,
    );
    lapack_status(info != 0, info, "zgeev")?;
    let lwork = query.r as i32;
    let mut work = vec![Rcomplex::default(); lwork.max(1) as usize];
    (lapack.zgeev)(
        c"N".as_ptr(),
        job_right.as_ptr(),
        &order,
        matrix.as_mut_ptr(),
        &order,
        complexes_mut(eigenvalues).as_mut_ptr(),
        std::ptr::null_mut(),
        &order,
        right_pointer,
        &order,
        work.as_mut_ptr(),
        &lwork,
        real_work.as_mut_ptr(),
        &mut info,
        1,
        1,
    );
    lapack_status(info != 0, info, "zgeev")?;
    match vectors {
        Some(vectors) => values::named_list(&[("values", eigenvalues), ("vectors", vectors)]),
        None => values::named_list(&[("values", eigenvalues)]),
    }
}

/// `determinant(x)`: the LU factorization's diagonal, as a modulus (or its log) and a sign.
unsafe fn det_ge_real(a: Sexp, logarithm: Sexp) -> R<Sexp> {
    let lapack = lapack()?;
    let use_log = values::as_logical_scalar(logarithm);
    if matrix_dims(a).is_none() {
        return Err(error("'a' must be a numeric matrix"));
    }
    if values::is_na_int(use_log) {
        return Err(error("argument 'logarithm' must be logical"));
    }
    let use_log = use_log != 0;
    let n = square_order(a, "'a' must be a square matrix")?;
    let mut factors = reals(coerce_vector(a, SexpType::Real)?).to_vec();
    let mut pivots = vec![0i32; n];
    let order = n as i32;
    let mut info = 0;
    (lapack.dgetrf)(
        &order,
        &order,
        factors.as_mut_ptr(),
        &order,
        pivots.as_mut_ptr(),
        &mut info,
    );
    let mut sign = 1;
    let modulus = if info < 0 {
        return Err(error(format!(
            "error code {info} from Lapack routine 'dgetrf'"
        )));
    } else if info > 0 {
        if use_log { f64::NEG_INFINITY } else { 0.0 }
    } else {
        for (index, &pivot) in pivots.iter().enumerate() {
            if pivot != index as i32 + 1 {
                sign = -sign;
            }
        }
        let diagonal = (0..n).map(|index| factors[index * (n + 1)]);
        if use_log {
            let mut modulus = 0.0;
            for entry in diagonal {
                modulus += entry.abs().ln();
                if entry < 0.0 {
                    sign = -sign;
                }
            }
            modulus
        } else {
            let mut modulus = 1.0;
            for entry in diagonal {
                modulus *= entry;
            }
            if modulus < 0.0 {
                modulus = -modulus;
                sign = -sign;
            }
            modulus
        }
    };
    let modulus = values::scalar_real(modulus)?;
    values::set_attr(
        modulus,
        values::install("logarithm")?,
        values::scalar_bool(use_log)?,
    )?;
    let result = values::named_list(&[
        ("modulus", modulus),
        ("sign", values::scalar_integer(sign)?),
    ])?;
    values::set_attr(result, symbols().class, values::scalar_string("det")?)?;
    Ok(result)
}

// Norms and condition numbers.

/// `norm(x, type)`: `dlange`.
unsafe fn la_dlange(a: Sexp, norm: Sexp) -> R<Sexp> {
    let lapack = lapack()?;
    let Some((rows, cols)) = matrix_dims(a) else {
        return Err(error("'A' must be a numeric matrix"));
    };
    if norm.kind() != SexpType::String {
        return Err(error("'type' must be a character string"));
    }
    let source = coerce_vector(a, SexpType::Real)?;
    let kind = norm_type(&first_text(norm), true)?;
    let (m, n) = (rows as i32, cols as i32);
    let mut work = vec![0.0; if kind == b'I' { rows.max(1) } else { 0 }];
    let work_pointer = if kind == b'I' {
        work.as_mut_ptr()
    } else {
        std::ptr::null_mut()
    };
    let kind = [kind as c_char, 0];
    let value = (lapack.dlange)(
        kind.as_ptr(),
        &m,
        &n,
        reals(source).as_ptr(),
        &m.max(1),
        work_pointer,
        1,
    );
    values::scalar_real(value)
}

/// `rcond(x)`: the LU factorization's reciprocal condition estimate, `dgecon`.
unsafe fn la_dgecon(a: Sexp, norm: Sexp) -> R<Sexp> {
    let lapack = lapack()?;
    let Some((rows, cols)) = matrix_dims(a) else {
        return Err(error("'A' must be a numeric matrix"));
    };
    if norm.kind() != SexpType::String {
        return Err(error("'norm' must be a character string"));
    }
    // GNU R factors an m x n matrix but then estimates with n as the leading dimension, which
    // reads past the matrix when m < n.
    if rows < cols {
        return Err(error("'A' must be a square matrix"));
    }
    let mut matrix = reals(coerce_vector(a, SexpType::Real)?).to_vec();
    let kind = norm_type(&first_text(norm), false)?;
    let (m, n) = (rows as i32, cols as i32);
    let mut work = vec![
        0.0;
        if kind == b'I' && rows > 4 * cols {
            rows
        } else {
            4 * cols
        }
        .max(1)
    ];
    let mut integer_work = vec![0i32; rows.max(1)];
    let kind = [kind as c_char, 0];
    let anorm = (lapack.dlange)(
        kind.as_ptr(),
        &m,
        &n,
        matrix.as_ptr(),
        &m.max(1),
        work.as_mut_ptr(),
        1,
    );
    let mut info = 0;
    (lapack.dgetrf)(
        &m,
        &n,
        matrix.as_mut_ptr(),
        &m.max(1),
        integer_work.as_mut_ptr(),
        &mut info,
    );
    if info < 0 {
        return Err(error(format!("error [{info}] from Lapack 'dgetrf()'")));
    }
    if info > 0 {
        return values::scalar_real(0.0);
    }
    let mut rcond = 0.0;
    (lapack.dgecon)(
        kind.as_ptr(),
        &n,
        matrix.as_ptr(),
        &n.max(1),
        &anorm,
        &mut rcond,
        work.as_mut_ptr(),
        integer_work.as_mut_ptr(),
        &mut info,
        1,
    );
    if info != 0 {
        return Err(error(format!("error [{info}] from Lapack 'dgecon()'")));
    }
    values::scalar_real(rcond)
}

/// `rcond(x, triangular = TRUE)`: `dtrcon` on the upper triangle.
unsafe fn la_dtrcon(a: Sexp, norm: Sexp, triangle: &CStr) -> R<Sexp> {
    let lapack = lapack()?;
    if matrix_dims(a).is_none() {
        return Err(error("'A' must be a numeric matrix"));
    }
    if norm.kind() != SexpType::String {
        return Err(error("'norm' must be a character string"));
    }
    let n = square_order(a, "'A' must be a *square* matrix")?;
    let matrix = coerce_vector(a, SexpType::Real)?;
    let kind = [norm_type(&first_text(norm), false)? as c_char, 0];
    let order = n as i32;
    let mut work = vec![0.0; 3 * n.max(1)];
    let mut integer_work = vec![0i32; n.max(1)];
    let mut rcond = 0.0;
    let mut info = 0;
    (lapack.dtrcon)(
        kind.as_ptr(),
        triangle.as_ptr(),
        c"N".as_ptr(),
        &order,
        reals(matrix).as_ptr(),
        &order.max(1),
        &mut rcond,
        work.as_mut_ptr(),
        integer_work.as_mut_ptr(),
        &mut info,
        1,
        1,
        1,
    );
    if info != 0 {
        return Err(error(format!("error [{info}] from Lapack 'dtrcon()'")));
    }
    values::scalar_real(rcond)
}

unsafe fn la_zgecon(a: Sexp, norm: Sexp) -> R<Sexp> {
    let lapack = lapack()?;
    if matrix_dims(a).is_none() {
        return Err(error("'A' must be a complex matrix"));
    }
    if norm.kind() != SexpType::String {
        return Err(error("'norm' must be a character string"));
    }
    let n = square_order(a, "'A' must be a square matrix")?;
    let kind = [norm_type(&first_text(norm), false)? as c_char, 0];
    let mut matrix = complexes(coerce_vector(a, SexpType::Complex)?).to_vec();
    let order = n as i32;
    let mut real_work = vec![0.0; 2 * n.max(1)];
    let anorm = (lapack.zlange)(
        kind.as_ptr(),
        &order,
        &order,
        matrix.as_ptr(),
        &order.max(1),
        real_work.as_mut_ptr(),
        1,
    );
    let mut pivots = vec![0i32; n.max(1)];
    let mut info = 0;
    (lapack.zgetrf)(
        &order,
        &order,
        matrix.as_mut_ptr(),
        &order.max(1),
        pivots.as_mut_ptr(),
        &mut info,
    );
    if info < 0 {
        return Err(error(format!("error [{info}] from Lapack 'zgetrf()'")));
    }
    if info > 0 {
        return values::scalar_real(0.0);
    }
    let mut work = vec![Rcomplex::default(); 4 * n.max(1)];
    let mut rcond = 0.0;
    (lapack.zgecon)(
        kind.as_ptr(),
        &order,
        matrix.as_ptr(),
        &order.max(1),
        &anorm,
        &mut rcond,
        work.as_mut_ptr(),
        real_work.as_mut_ptr(),
        &mut info,
        1,
    );
    if info != 0 {
        return Err(error(format!("error [{info}] from Lapack 'zgecon()'")));
    }
    values::scalar_real(rcond)
}

unsafe fn la_ztrcon(a: Sexp, norm: Sexp, triangle: &CStr) -> R<Sexp> {
    let lapack = lapack()?;
    if matrix_dims(a).is_none() {
        return Err(error("'A' must be a complex matrix"));
    }
    if norm.kind() != SexpType::String {
        return Err(error("'norm' must be a character string"));
    }
    let n = square_order(a, "'A' must be a square matrix")?;
    let kind = [norm_type(&first_text(norm), false)? as c_char, 0];
    let matrix = coerce_vector(a, SexpType::Complex)?;
    let order = n as i32;
    let mut work = vec![Rcomplex::default(); 2 * n.max(1)];
    let mut real_work = vec![0.0; n.max(1)];
    let mut rcond = 0.0;
    let mut info = 0;
    (lapack.ztrcon)(
        kind.as_ptr(),
        triangle.as_ptr(),
        c"N".as_ptr(),
        &order,
        complexes(matrix).as_ptr(),
        &order.max(1),
        &mut rcond,
        work.as_mut_ptr(),
        real_work.as_mut_ptr(),
        &mut info,
        1,
        1,
        1,
    );
    if info != 0 {
        return Err(error(format!("error [{info}] from Lapack 'ztrcon()'")));
    }
    values::scalar_real(rcond)
}

/// The triangle `uplo` names, `"U"` or `"L"`.
unsafe fn uplo(value: Sexp) -> R<&'static CStr> {
    match values::first_string(value).as_deref() {
        Some("U") => Ok(c"U"),
        Some("L") => Ok(c"L"),
        _ => Err(error("'uplo' must be \"U\" or \"L\"")),
    }
}

/// `norm(x, type)` of a complex matrix, `zlange`.
unsafe fn la_zlange(a: Sexp, norm: Sexp) -> R<Sexp> {
    let lapack = lapack()?;
    let Some((rows, cols)) = matrix_dims(a) else {
        return Err(error("'A' must be a complex matrix"));
    };
    if norm.kind() != SexpType::String {
        return Err(error("'type' must be a character string"));
    }
    let source = coerce_vector(a, SexpType::Complex)?;
    let kind = norm_type(&first_text(norm), true)?;
    let (m, n) = (rows as i32, cols as i32);
    let mut work = vec![0.0; rows.max(1)];
    let kind = [kind as c_char, 0];
    let value = (lapack.zlange)(
        kind.as_ptr(),
        &m,
        &n,
        complexes(source).as_ptr(),
        &m.max(1),
        work.as_mut_ptr(),
        1,
    );
    values::scalar_real(value)
}

/// The LAPACK norm letter `type` names: `M`, `O` (or `1`), `I`, `F` (or `E`); condition
/// estimates take only the one and infinity norms.
fn norm_type(text: &str, any_norm: bool) -> R<u8> {
    let bytes = text.as_bytes();
    if bytes.len() != 1 {
        return Err(error(format!(
            "argument type[1]='{text}' must be a character string of string length 1"
        )));
    }
    let letter = match bytes[0].to_ascii_uppercase() {
        b'1' => b'O',
        b'E' if any_norm => b'F',
        letter => letter,
    };
    let allowed: &[u8] = if any_norm { b"MOIF" } else { b"OI" };
    if !allowed.contains(&letter) {
        return Err(error(if any_norm {
            format!("argument type[1]='{text}' must be one of 'M','1','O','I','F' or 'E'")
        } else {
            format!("argument type[1]='{text}' must be one of '1','O', or 'I'")
        }));
    }
    Ok(letter)
}

// `backsolve` and the library information.

/// `backsolve(r, x, k, upper.tri, transpose)`: `dtrsm` on the leading `k` rows and columns.
unsafe fn backsolve(call: &Call<'_>) -> R<Sexp> {
    let blas = blas()?;
    let (r, x) = (call.arg(0), call.arg(1));
    let (r_rows, r_cols) = matrix_dims(r).unwrap_or((length(r), 1));
    let (x_rows, x_cols) = matrix_dims(x).unwrap_or((length(x), 1));
    let k = values::as_integer_scalar(call.arg(2));
    if values::is_na_int(k)
        || k <= 0
        || k as usize > r_rows
        || k as usize > r_cols
        || k as usize > x_rows
    {
        return Err(error("invalid 'k' argument"));
    }
    let k = k as usize;
    let upper = values::as_logical_scalar(call.arg(3));
    if values::is_na_int(upper) {
        return Err(error("invalid 'upper.tri' argument"));
    }
    let transpose = values::as_logical_scalar(call.arg(4));
    if values::is_na_int(transpose) {
        return Err(error("invalid 'transpose' argument"));
    }
    let r_values = coerce_vector(r, SexpType::Real)?;
    let x_values = coerce_vector(x, SexpType::Real)?;
    let triangle = reals(r_values);
    for index in 0..k {
        if triangle[index * (r_rows + 1)] == 0.0 {
            return Err(error(format!(
                "singular matrix in 'backsolve'. First zero in diagonal [{}]",
                index + 1
            )));
        }
    }
    let result = alloc_matrix(SexpType::Real, k, x_cols)?;
    if x_cols > 0 {
        let output = reals_mut(result);
        let input = reals(x_values);
        for col in 0..x_cols {
            output[col * k..(col + 1) * k].copy_from_slice(&input[col * x_rows..col * x_rows + k]);
        }
        let (order, columns, leading) = (k as i32, x_cols as i32, r_rows as i32);
        let one = 1.0;
        let uplo = if upper != 0 { c"U" } else { c"L" };
        let trans = if transpose != 0 { c"T" } else { c"N" };
        (blas.dtrsm)(
            c"L".as_ptr(),
            uplo.as_ptr(),
            trans.as_ptr(),
            c"N".as_ptr(),
            &order,
            &columns,
            &one,
            triangle.as_ptr(),
            &leading,
            output.as_mut_ptr(),
            &order,
            1,
            1,
            1,
            1,
        );
    }
    Ok(result)
}

unsafe fn la_version() -> R<Sexp> {
    let lapack = lapack()?;
    let (mut major, mut minor, mut patch) = (0, 0, 0);
    (lapack.ilaver)(&mut major, &mut minor, &mut patch);
    values::scalar_string(&format!("{major}.{minor}.{patch}"))
}

/// The file the LAPACK routines came from, with symbolic links resolved, as GNU R reports it.
unsafe fn la_library() -> R<Sexp> {
    let lapack = lapack()?;
    let mut info: libc::Dl_info = std::mem::zeroed();
    let address = lapack.ilaver as *const c_void;
    if libc::dladdr(address, &mut info) == 0 || info.dli_fname.is_null() {
        return values::scalar_string("");
    }
    let path = CStr::from_ptr(info.dli_fname)
        .to_string_lossy()
        .into_owned();
    let resolved = std::fs::canonicalize(&path)
        .map_or(path, |resolved| resolved.to_string_lossy().into_owned());
    values::scalar_string(&resolved)
}

// The LINPACK routines base registers for `.Fortran`. Each receives the copies `.Fortran` made
// of its arguments and updates them in place.

/// `dqrdc2(x, ldx, n, p, tol, rank, qraux, pivot, work)`: the QR decomposition `qr()` computes
/// by default.
pub unsafe fn dqrdc2(args: &[Sexp]) -> R<()> {
    let blas = blas()?;
    let ldx = integer_arg(args, 1)?;
    let n = integer_arg(args, 2)?;
    let p = integer_arg(args, 3)?;
    let tol = real_arg(args, 4)?;
    if ldx < n.max(1) {
        return Err(error("invalid 'ldx' argument to 'dqrdc2'"));
    }
    let x = real_slice(args, 0, ldx * p)?;
    let rank = integer_slice(args, 5, 1)?;
    let qraux = real_slice(args, 6, p)?;
    let pivot = integer_slice(args, 7, p)?;
    let work = real_slice(args, 8, 2 * p)?;
    let (downdated_norms, original_norms) = work.split_at_mut(p);
    for col in 0..p {
        let norm = blas.nrm2(&x[col * ldx..col * ldx + n]);
        qraux[col] = norm;
        downdated_norms[col] = norm;
        original_norms[col] = if norm == 0.0 { 1.0 } else { norm };
    }
    // `kept` counts the columns not yet moved to the end, plus one.
    let mut kept = p + 1;
    for l in 0..n.min(p) {
        while l + 1 < kept && qraux[l] < original_norms[l] * tol {
            for row in 0..n {
                let moved = x[row + l * ldx];
                for col in l + 1..p {
                    x[row + (col - 1) * ldx] = x[row + col * ldx];
                }
                x[row + (p - 1) * ldx] = moved;
            }
            pivot[l..p].rotate_left(1);
            qraux[l..p].rotate_left(1);
            downdated_norms[l..p].rotate_left(1);
            original_norms[l..p].rotate_left(1);
            kept -= 1;
        }
        if l + 1 == n {
            break;
        }
        let diagonal = l + l * ldx;
        let mut norm = blas.nrm2(&x[diagonal..l * ldx + n]);
        if norm == 0.0 {
            continue;
        }
        if x[diagonal] != 0.0 {
            norm = norm.copysign(x[diagonal]);
        }
        blas.scal(1.0 / norm, &mut x[diagonal..l * ldx + n]);
        x[diagonal] += 1.0;
        for col in l + 1..p {
            let (left, right) = x.split_at_mut(col * ldx);
            let reflector = &left[diagonal..l * ldx + n];
            let target = &mut right[l..n];
            let t = -blas.dot(reflector, target) / reflector[0];
            blas.axpy(t, reflector, target);
            if qraux[col] != 0.0 {
                let ratio = target[0].abs() / qraux[col];
                let remaining = (1.0 - ratio * ratio).max(0.0);
                // R's change to LINPACK: recompute the norm instead of downdating it once
                // cancellation has eaten most of its digits.
                if remaining.abs() < 1e-6 {
                    qraux[col] = blas.nrm2(&target[1..]);
                    downdated_norms[col] = qraux[col];
                } else {
                    qraux[col] *= remaining.sqrt();
                }
            }
        }
        qraux[l] = x[diagonal];
        x[diagonal] = -norm;
    }
    rank[0] = (kept - 1).min(n) as i32;
    Ok(())
}

/// `dqrqty(qr, n, k, qraux, y, ny, qty)`: `Q'y` for each column of `y`.
pub unsafe fn dqrqty(args: &[Sexp]) -> R<()> {
    let mut problem = QrProblem::read(args)?;
    let size = problem.n * problem.columns;
    let (y, output) = (real_slice(args, 4, size)?, real_slice(args, 6, size)?);
    for col in 0..problem.columns {
        let column = problem.column(col);
        let mut outputs = QrOutputs {
            qty: Some(&mut output[column.clone()]),
            ..QrOutputs::default()
        };
        problem.solve(&y[column], &mut outputs);
    }
    Ok(())
}

/// `dqrqy(qr, n, k, qraux, y, ny, qy)`: `Qy` for each column of `y`.
pub unsafe fn dqrqy(args: &[Sexp]) -> R<()> {
    let mut problem = QrProblem::read(args)?;
    let size = problem.n * problem.columns;
    let (y, output) = (real_slice(args, 4, size)?, real_slice(args, 6, size)?);
    for col in 0..problem.columns {
        let column = problem.column(col);
        let mut outputs = QrOutputs {
            qy: Some(&mut output[column.clone()]),
            ..QrOutputs::default()
        };
        problem.solve(&y[column], &mut outputs);
    }
    Ok(())
}

/// `dqrcf(qr, n, k, qraux, y, ny, b, info)`: least-squares coefficients for each column of `y`,
/// which is overwritten with `Q'y`.
pub unsafe fn dqrcf(args: &[Sexp]) -> R<()> {
    let mut problem = QrProblem::read(args)?;
    let (n, k) = (problem.n, problem.k);
    let y = real_slice(args, 4, n * problem.columns)?;
    let coefficients = real_slice(args, 6, k * problem.columns)?;
    let info = integer_slice(args, 7, 1)?;
    for col in 0..problem.columns {
        let column = problem.column(col);
        let input = y[column.clone()].to_vec();
        let mut outputs = QrOutputs {
            qty: Some(&mut y[column]),
            b: Some(&mut coefficients[col * k..(col + 1) * k]),
            ..QrOutputs::default()
        };
        info[0] = problem.solve(&input, &mut outputs);
    }
    Ok(())
}

/// `dqrrsd(qr, n, k, qraux, y, ny, rsd)`: least-squares residuals; `y` becomes `Q'y`.
pub unsafe fn dqrrsd(args: &[Sexp]) -> R<()> {
    let mut problem = QrProblem::read(args)?;
    let size = problem.n * problem.columns;
    let (y, residuals) = (real_slice(args, 4, size)?, real_slice(args, 6, size)?);
    for col in 0..problem.columns {
        let column = problem.column(col);
        let input = y[column.clone()].to_vec();
        let mut outputs = QrOutputs {
            qty: Some(&mut y[column.clone()]),
            rsd: Some(&mut residuals[column]),
            ..QrOutputs::default()
        };
        problem.solve(&input, &mut outputs);
    }
    Ok(())
}

/// `dqrxb(qr, n, k, qraux, y, ny, xb)`: fitted values; `y` becomes `Q'y`.
pub unsafe fn dqrxb(args: &[Sexp]) -> R<()> {
    let mut problem = QrProblem::read(args)?;
    let size = problem.n * problem.columns;
    let (y, fitted) = (real_slice(args, 4, size)?, real_slice(args, 6, size)?);
    for col in 0..problem.columns {
        let column = problem.column(col);
        let input = y[column.clone()].to_vec();
        let mut outputs = QrOutputs {
            qty: Some(&mut y[column.clone()]),
            xb: Some(&mut fitted[column]),
            ..QrOutputs::default()
        };
        problem.solve(&input, &mut outputs);
    }
    Ok(())
}

/// The shared arguments of the `dqr*` routines: a compact QR factor from `dqrdc2` with `n`
/// rows and rank `k`, and the number of right-hand side columns.
struct QrProblem<'a> {
    blas: &'static Blas,
    qr: &'a mut [f64],
    n: usize,
    k: usize,
    qraux: &'a [f64],
    columns: usize,
}

/// Which products [`QrProblem::solve`] computes: LINPACK `dqrsl`'s `job`.
#[derive(Default)]
struct QrOutputs<'a> {
    qy: Option<&'a mut [f64]>,
    qty: Option<&'a mut [f64]>,
    b: Option<&'a mut [f64]>,
    rsd: Option<&'a mut [f64]>,
    xb: Option<&'a mut [f64]>,
}

impl<'a> QrProblem<'a> {
    unsafe fn read(args: &[Sexp]) -> R<QrProblem<'a>> {
        let blas = blas()?;
        let n = integer_arg(args, 1)?;
        let k = integer_arg(args, 2)?;
        let columns = integer_arg(args, 5)?;
        if k > n {
            return Err(error(
                "the rank of the QR decomposition exceeds its row count",
            ));
        }
        Ok(QrProblem {
            blas,
            qr: real_slice(args, 0, n * k)?,
            n,
            k,
            qraux: real_slice(args, 3, k)?,
            columns,
        })
    }

    fn column(&self, col: usize) -> std::ops::Range<usize> {
        col * self.n..(col + 1) * self.n
    }

    /// LINPACK's `dqrsl` for one right-hand side `y`. Returns `info`: 0, or the index of the
    /// first zero diagonal element of `R` met while solving for `b`. `b`, `rsd` and `xb` are
    /// built from `Q'y`, so asking for one of them computes `qty` too.
    fn solve(&mut self, y: &[f64], outputs: &mut QrOutputs<'_>) -> i32 {
        let QrProblem {
            blas,
            qr,
            n,
            k,
            qraux,
            ..
        } = self;
        let (n, k) = (*n, *k);
        if n == 0 {
            return 0;
        }
        let mut info = 0;
        let reflectors = k.min(n - 1);
        if reflectors == 0 {
            // LINPACK's special case for a single row (or rank zero) sets only the first
            // element of each output.
            for output in [&mut outputs.qy, &mut outputs.qty, &mut outputs.xb]
                .into_iter()
                .flatten()
            {
                output[0] = y[0];
            }
            if let Some(b) = outputs.b.as_deref_mut() {
                if qr[0] == 0.0 {
                    info = 1;
                } else {
                    b[0] = y[0] / qr[0];
                }
            }
            if let Some(rsd) = outputs.rsd.as_deref_mut() {
                rsd[0] = 0.0;
            }
            return info;
        }
        if let Some(qy) = outputs.qy.as_deref_mut() {
            qy.copy_from_slice(y);
            for j in (0..reflectors).rev() {
                reflect(blas, qr, n, qraux[j], j, qy);
            }
        }
        let wants_qty = outputs.qty.is_some()
            || outputs.b.is_some()
            || outputs.rsd.is_some()
            || outputs.xb.is_some();
        if !wants_qty {
            return info;
        }
        let mut local_qty = Vec::new();
        let qty: &mut [f64] = match outputs.qty.as_deref_mut() {
            Some(qty) => qty,
            None => {
                local_qty.resize(n, 0.0);
                &mut local_qty
            }
        };
        qty.copy_from_slice(y);
        for j in 0..reflectors {
            reflect(blas, qr, n, qraux[j], j, qty);
        }
        if let Some(b) = outputs.b.as_deref_mut() {
            b.copy_from_slice(&qty[..k]);
        }
        if let Some(xb) = outputs.xb.as_deref_mut() {
            xb[..k].copy_from_slice(&qty[..k]);
            xb[k..].fill(0.0);
        }
        if let Some(rsd) = outputs.rsd.as_deref_mut() {
            rsd[k..].copy_from_slice(&qty[k..]);
            rsd[..k].fill(0.0);
        }
        if let Some(b) = outputs.b.as_deref_mut() {
            for j in (0..k).rev() {
                let pivot = qr[j + j * n];
                if pivot == 0.0 {
                    info = j as i32 + 1;
                    break;
                }
                b[j] /= pivot;
                if j > 0 {
                    let (head, tail) = b.split_at_mut(j);
                    blas.axpy(-tail[0], &qr[j * n..j * n + j], head);
                }
            }
        }
        for j in (0..reflectors).rev() {
            if let Some(rsd) = outputs.rsd.as_deref_mut() {
                reflect(blas, qr, n, qraux[j], j, rsd);
            }
            if let Some(xb) = outputs.xb.as_deref_mut() {
                reflect(blas, qr, n, qraux[j], j, xb);
            }
        }
        info
    }
}

/// Applies the Householder reflector of column `j` to rows `j..` of `target`. The reflector is
/// stored below the diagonal with its leading element in `leading` (`qraux[j]`), so the
/// diagonal holds that element for the duration.
fn reflect(blas: &Blas, qr: &mut [f64], n: usize, leading: f64, j: usize, target: &mut [f64]) {
    if leading == 0.0 {
        return;
    }
    let diagonal = j + j * n;
    let saved = qr[diagonal];
    qr[diagonal] = leading;
    let reflector = &qr[diagonal..j * n + n];
    let t = -blas.dot(reflector, &target[j..]) / reflector[0];
    blas.axpy(t, reflector, &mut target[j..]);
    qr[diagonal] = saved;
}

/// `dtrco(t, ldt, n, rcond, z, job)`: LINPACK's estimate of a triangular matrix's reciprocal
/// condition number in the one norm (`job` 0 for lower, 1 for upper triangular), which
/// `kappa()` reports.
pub unsafe fn dtrco(args: &[Sexp]) -> R<()> {
    let blas = blas()?;
    let ldt = integer_arg(args, 1)?;
    let n = integer_arg(args, 2)?;
    if ldt < n.max(1) {
        return Err(error("invalid 'ldt' argument to 'dtrco'"));
    }
    let t = real_slice(args, 0, ldt * n)?;
    let rcond = real_slice(args, 3, 1)?;
    let z = real_slice(args, 4, n)?;
    let lower = integer_arg(args, 5)? == 0;
    let at = |row: usize, col: usize| t[row + col * ldt];
    let mut t_norm = 0.0f64;
    for col in 0..n {
        let (start, count) = if lower { (col, n - col) } else { (0, col + 1) };
        t_norm = t_norm.max(blas.asum(&t[start + col * ldt..start + col * ldt + count]));
    }
    // Solve t' y = e, choosing each e entry as +1 or -1 to make y grow.
    let mut ek = 1.0f64;
    z.fill(0.0);
    for step in 0..n {
        let k = if lower { n - 1 - step } else { step };
        if z[k] != 0.0 {
            ek = ek.abs().copysign(-z[k]);
        }
        if (ek - z[k]).abs() > at(k, k).abs() {
            let s = at(k, k).abs() / (ek - z[k]).abs();
            blas.scal(s, z);
            ek *= s;
        }
        let mut wk = ek - z[k];
        let mut wkm = -ek - z[k];
        let mut s = wk.abs();
        let mut sm = wkm.abs();
        if at(k, k) != 0.0 {
            wk /= at(k, k);
            wkm /= at(k, k);
        } else {
            wk = 1.0;
            wkm = 1.0;
        }
        if step + 1 != n {
            let range = if lower { 0..k } else { k + 1..n };
            for j in range.clone() {
                sm += (z[j] + wkm * at(k, j)).abs();
                z[j] += wk * at(k, j);
                s += z[j].abs();
            }
            if s < sm {
                let w = wkm - wk;
                wk = wkm;
                for j in range {
                    z[j] += w * at(k, j);
                }
            }
        }
        z[k] = wk;
    }
    let s = 1.0 / blas.asum(z);
    blas.scal(s, z);
    let mut y_norm = 1.0f64;
    // Solve t z = y.
    for step in 0..n {
        let k = if lower { step } else { n - 1 - step };
        if z[k].abs() > at(k, k).abs() {
            let s = at(k, k).abs() / z[k].abs();
            blas.scal(s, z);
            y_norm *= s;
        }
        if at(k, k) != 0.0 {
            z[k] /= at(k, k);
        } else {
            z[k] = 1.0;
        }
        if step + 1 < n {
            let start = if lower { k + 1 } else { 0 };
            let w = -z[k];
            let column = &t[start + k * ldt..start + k * ldt + (n - step - 1)];
            blas.axpy(w, column, &mut z[start..start + n - step - 1]);
        }
    }
    let s = 1.0 / blas.asum(z);
    blas.scal(s, z);
    y_norm *= s;
    rcond[0] = if t_norm != 0.0 { y_norm / t_norm } else { 0.0 };
    Ok(())
}

unsafe fn integer_arg(args: &[Sexp], index: usize) -> R<usize> {
    let value = args
        .get(index)
        .copied()
        .filter(|value| value.kind() == SexpType::Integer && length(*value) >= 1);
    match value.map(|value| ints(value)[0]) {
        Some(value) if value >= 0 => Ok(value as usize),
        _ => Err(error(format!(
            "argument {} must be a non-negative integer",
            index + 1
        ))),
    }
}

unsafe fn real_arg(args: &[Sexp], index: usize) -> R<f64> {
    match args
        .get(index)
        .copied()
        .filter(|value| value.kind() == SexpType::Real && length(*value) >= 1)
    {
        Some(value) => Ok(reals(value)[0]),
        None => Err(error(format!("argument {} must be a double", index + 1))),
    }
}

/// Argument `index` as at least `count` doubles: the bounds a Fortran routine silently assumes.
unsafe fn real_slice<'a>(args: &[Sexp], index: usize, count: usize) -> R<&'a mut [f64]> {
    match args
        .get(index)
        .copied()
        .filter(|value| value.kind() == SexpType::Real)
    {
        Some(value) if length(value) >= count => Ok(&mut reals_mut(value)[..count]),
        _ => Err(error(format!(
            "argument {} must be a double vector of length {count} or more",
            index + 1
        ))),
    }
}

unsafe fn integer_slice<'a>(args: &[Sexp], index: usize, count: usize) -> R<&'a mut [i32]> {
    match args
        .get(index)
        .copied()
        .filter(|value| value.kind() == SexpType::Integer)
    {
        Some(value) if length(value) >= count => Ok(&mut ints_mut(value)[..count]),
        _ => Err(error(format!(
            "argument {} must be an integer vector of length {count} or more",
            index + 1
        ))),
    }
}

// Shared helpers.

unsafe fn is_numeric_or_complex(x: Sexp) -> bool {
    match x.kind() {
        SexpType::Integer => !values::inherits(x, "factor"),
        SexpType::Logical | SexpType::Real | SexpType::Complex => true,
        _ => false,
    }
}

/// `x`'s rows and columns when it is a matrix: a vector with a two-element `dim`.
unsafe fn matrix_dims(x: Sexp) -> Option<(usize, usize)> {
    if !x.kind().is_vector() {
        return None;
    }
    match dims(x)?.as_slice() {
        &[rows, cols] => Some((rows, cols)),
        _ => None,
    }
}

unsafe fn alloc_matrix(kind: SexpType, rows: usize, cols: usize) -> R<Sexp> {
    let result = alloc(kind, rows * cols)?;
    let dim = values::integer_vector(&[rows as i32, cols as i32])?;
    values::set_attr(result, symbols().dim, dim)?;
    Ok(result)
}

/// A copy of `x` as `kind` that LAPACK may overwrite, keeping its attributes.
unsafe fn fresh_copy(x: Sexp, kind: SexpType) -> R<Sexp> {
    if x.kind() == kind {
        values::duplicate(x)
    } else {
        coerce_vector(x, kind)
    }
}

unsafe fn list_or_nil(list: Sexp, index: usize) -> Sexp {
    if list.kind() == SexpType::List && length(list) > index {
        list.element(index)
    } else {
        nil()
    }
}

unsafe fn first_text(strings: Sexp) -> String {
    values::first_string(strings)
        .map(|text| text.into_owned())
        .unwrap_or_else(|| "NA".to_owned())
}

/// Reorders the column names of a pivoted decomposition to follow its columns.
unsafe fn permute_column_names(matrix: Sexp, pivots: &[i32]) -> R<()> {
    let dimnames = get_attr(matrix, symbols().dimnames);
    let names = list_or_nil(dimnames, 1);
    if names == nil() {
        return Ok(());
    }
    let permuted = alloc(SexpType::String, pivots.len())?;
    for (index, &pivot) in pivots.iter().enumerate() {
        let source = (pivot as usize)
            .saturating_sub(1)
            .min(length(names).saturating_sub(1));
        values::set_elt(permuted, index, names.element(source));
    }
    let dimnames = values::shallow_duplicate(dimnames)?;
    values::set_elt(dimnames, 1, permuted);
    values::set_attr(matrix, symbols().dimnames, dimnames)
}

fn lapack_status(failed: bool, info: i32, routine: &str) -> R<()> {
    if failed {
        return Err(error(format!(
            "error code {info} from Lapack routine '{routine}'"
        )));
    }
    Ok(())
}

/// `%g` with six significant digits, as GNU R's messages print reals.
fn format_g(value: f64) -> String {
    let mut buffer = [0u8; 64];
    // SAFETY: the format takes one double and the buffer is large enough for any `%g` output.
    let written = unsafe {
        libc::snprintf(
            buffer.as_mut_ptr().cast(),
            buffer.len(),
            c"%g".as_ptr(),
            value,
        )
    };
    String::from_utf8_lossy(&buffer[..written.clamp(0, 63) as usize]).into_owned()
}

// The libraries. Each table is opened on first use; a missing library is an error only for the
// operations that need it.

type Integer = *const i32;
type IntegerOut = *mut i32;
type Double = *const f64;
type DoubleOut = *mut f64;
type Complex = *const Rcomplex;
type ComplexOut = *mut Rcomplex;
type Text = *const c_char;
/// The hidden length gfortran passes after the arguments for each character argument.
type TextLength = usize;

macro_rules! fortran_library {
    ($library:ident, $description:literal, [$($candidate:literal),*], {
        $($routine:ident: fn($($argument:ty),*) $(-> $result:ty)?;)*
    }) => {
        struct $library {
            $($routine: unsafe extern "C" fn($($argument),*) $(-> $result)?,)*
        }

        impl $library {
            unsafe fn open() -> Result<$library, String> {
                let handle = open_library(&[$($candidate),*], $description)?;
                Ok($library {
                    $($routine: std::mem::transmute::<*mut c_void, unsafe extern "C" fn($($argument),*) $(-> $result)?>(
                        library_symbol(handle, concat!(stringify!($routine), "_\0"), $description)?,
                    ),)*
                })
            }
        }
    };
}

fortran_library!(Blas, "BLAS", ["libblas.so.3", "libblas.so", "libblas.dylib"], {
    dgemm: fn(Text, Text, Integer, Integer, Integer, Double, Double, Integer, Double, Integer, Double, DoubleOut, Integer, TextLength, TextLength);
    dgemv: fn(Text, Integer, Integer, Double, Double, Integer, Double, Integer, Double, DoubleOut, Integer, TextLength);
    dsyrk: fn(Text, Text, Integer, Integer, Double, Double, Integer, Double, DoubleOut, Integer, TextLength, TextLength);
    zgemm: fn(Text, Text, Integer, Integer, Integer, Complex, Complex, Integer, Complex, Integer, Complex, ComplexOut, Integer, TextLength, TextLength);
    dtrsm: fn(Text, Text, Text, Text, Integer, Integer, Double, Double, Integer, DoubleOut, Integer, TextLength, TextLength, TextLength, TextLength);
    dnrm2: fn(Integer, Double, Integer) -> f64;
    ddot: fn(Integer, Double, Integer, Double, Integer) -> f64;
    dasum: fn(Integer, Double, Integer) -> f64;
    daxpy: fn(Integer, Double, Double, Integer, DoubleOut, Integer);
    dscal: fn(Integer, Double, DoubleOut, Integer);
});

fortran_library!(Lapack, "LAPACK", ["liblapack.so.3", "liblapack.so", "liblapack.dylib"], {
    ilaver: fn(IntegerOut, IntegerOut, IntegerOut);
    dgesv: fn(Integer, Integer, DoubleOut, Integer, IntegerOut, DoubleOut, Integer, IntegerOut);
    dgetrf: fn(Integer, Integer, DoubleOut, Integer, IntegerOut, IntegerOut);
    dgecon: fn(Text, Integer, Double, Integer, Double, DoubleOut, DoubleOut, IntegerOut, IntegerOut, TextLength);
    dlange: fn(Text, Integer, Integer, Double, Integer, DoubleOut, TextLength) -> f64;
    dtrcon: fn(Text, Text, Text, Integer, Double, Integer, DoubleOut, DoubleOut, IntegerOut, IntegerOut, TextLength, TextLength, TextLength);
    dpotrf: fn(Text, Integer, DoubleOut, Integer, IntegerOut, TextLength);
    dpotri: fn(Text, Integer, DoubleOut, Integer, IntegerOut, TextLength);
    dpstrf: fn(Text, Integer, DoubleOut, Integer, IntegerOut, IntegerOut, Double, DoubleOut, IntegerOut, TextLength);
    dgeqp3: fn(Integer, Integer, DoubleOut, Integer, IntegerOut, DoubleOut, DoubleOut, Integer, IntegerOut);
    dormqr: fn(Text, Text, Integer, Integer, Integer, Double, Integer, Double, DoubleOut, Integer, DoubleOut, Integer, IntegerOut, TextLength, TextLength);
    dtrtrs: fn(Text, Text, Text, Integer, Integer, Double, Integer, DoubleOut, Integer, IntegerOut, TextLength, TextLength, TextLength);
    dgesdd: fn(Text, Integer, Integer, DoubleOut, Integer, DoubleOut, DoubleOut, Integer, DoubleOut, Integer, DoubleOut, Integer, IntegerOut, IntegerOut, TextLength);
    dsyevr: fn(Text, Text, Text, Integer, DoubleOut, Integer, Double, Double, Integer, Integer, Double, IntegerOut, DoubleOut, DoubleOut, Integer, IntegerOut, DoubleOut, Integer, IntegerOut, Integer, IntegerOut, TextLength, TextLength, TextLength);
    dgeev: fn(Text, Text, Integer, DoubleOut, Integer, DoubleOut, DoubleOut, DoubleOut, Integer, DoubleOut, Integer, DoubleOut, Integer, IntegerOut, TextLength, TextLength);
    zgesv: fn(Integer, Integer, ComplexOut, Integer, IntegerOut, ComplexOut, Integer, IntegerOut);
    zgetrf: fn(Integer, Integer, ComplexOut, Integer, IntegerOut, IntegerOut);
    zgecon: fn(Text, Integer, Complex, Integer, Double, DoubleOut, ComplexOut, DoubleOut, IntegerOut, TextLength);
    zlange: fn(Text, Integer, Integer, Complex, Integer, DoubleOut, TextLength) -> f64;
    ztrcon: fn(Text, Text, Text, Integer, Complex, Integer, DoubleOut, ComplexOut, DoubleOut, IntegerOut, TextLength, TextLength, TextLength);
    zgeqp3: fn(Integer, Integer, ComplexOut, Integer, IntegerOut, ComplexOut, ComplexOut, Integer, DoubleOut, IntegerOut);
    zunmqr: fn(Text, Text, Integer, Integer, Integer, Complex, Integer, Complex, ComplexOut, Integer, ComplexOut, Integer, IntegerOut, TextLength, TextLength);
    ztrtrs: fn(Text, Text, Text, Integer, Integer, Complex, Integer, ComplexOut, Integer, IntegerOut, TextLength, TextLength, TextLength);
    zgesdd: fn(Text, Integer, Integer, ComplexOut, Integer, DoubleOut, ComplexOut, Integer, ComplexOut, Integer, ComplexOut, Integer, DoubleOut, IntegerOut, IntegerOut, TextLength);
    zheev: fn(Text, Text, Integer, ComplexOut, Integer, DoubleOut, ComplexOut, Integer, DoubleOut, IntegerOut, TextLength, TextLength);
    zgeev: fn(Text, Text, Integer, ComplexOut, Integer, ComplexOut, ComplexOut, Integer, ComplexOut, Integer, ComplexOut, Integer, DoubleOut, IntegerOut, TextLength, TextLength);
});

/// The level 1 routines LINPACK is written in, over whole slices.
impl Blas {
    fn nrm2(&self, x: &[f64]) -> f64 {
        // SAFETY: the routine reads `x.len()` doubles from `x`.
        unsafe { (self.dnrm2)(&(x.len() as i32), x.as_ptr(), &1) }
    }

    fn dot(&self, x: &[f64], y: &[f64]) -> f64 {
        let count = x.len().min(y.len()) as i32;
        // SAFETY: the routine reads `count` doubles from each slice.
        unsafe { (self.ddot)(&count, x.as_ptr(), &1, y.as_ptr(), &1) }
    }

    fn asum(&self, x: &[f64]) -> f64 {
        // SAFETY: the routine reads `x.len()` doubles from `x`.
        unsafe { (self.dasum)(&(x.len() as i32), x.as_ptr(), &1) }
    }

    fn axpy(&self, alpha: f64, x: &[f64], y: &mut [f64]) {
        let count = x.len().min(y.len()) as i32;
        // SAFETY: the routine reads `count` doubles from `x` and updates `count` in `y`.
        unsafe { (self.daxpy)(&count, &alpha, x.as_ptr(), &1, y.as_mut_ptr(), &1) }
    }

    fn scal(&self, alpha: f64, x: &mut [f64]) {
        // SAFETY: the routine scales `x.len()` doubles in place.
        unsafe { (self.dscal)(&(x.len() as i32), &alpha, x.as_mut_ptr(), &1) }
    }
}

static BLAS: OnceLock<Result<Blas, String>> = OnceLock::new();
static LAPACK: OnceLock<Result<Lapack, String>> = OnceLock::new();

fn blas() -> R<&'static Blas> {
    // SAFETY: opening a library and reading its symbols has no preconditions.
    let library = BLAS.get_or_init(|| unsafe { Blas::open() });
    library.as_ref().map_err(|message| error(message.clone()))
}

fn lapack() -> R<&'static Lapack> {
    // SAFETY: as for `blas`.
    let library = LAPACK.get_or_init(|| unsafe { Lapack::open() });
    library.as_ref().map_err(|message| error(message.clone()))
}

unsafe fn open_library(candidates: &[&str], description: &str) -> Result<*mut c_void, String> {
    let mut failure = String::new();
    for candidate in candidates {
        let name = std::ffi::CString::new(*candidate)
            .map_err(|_| format!("invalid library name {candidate}"))?;
        let handle = libc::dlopen(name.as_ptr(), libc::RTLD_NOW | libc::RTLD_GLOBAL);
        if !handle.is_null() {
            return Ok(handle);
        }
        if failure.is_empty() {
            let message = libc::dlerror();
            if !message.is_null() {
                failure = CStr::from_ptr(message).to_string_lossy().into_owned();
            }
        }
    }
    Err(format!(
        "linear algebra needs the system {description} library ({}), which could not be loaded: {failure}",
        candidates.first().copied().unwrap_or_default()
    ))
}

unsafe fn library_symbol(
    handle: *mut c_void,
    name: &str,
    description: &str,
) -> Result<*mut c_void, String> {
    let symbol = libc::dlsym(handle, name.as_ptr().cast());
    if symbol.is_null() {
        return Err(format!(
            "the system {description} library has no routine '{}'",
            name.trim_end_matches(['_', '\0'])
        ));
    }
    Ok(symbol)
}
