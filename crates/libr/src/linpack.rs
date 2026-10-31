//! The LINPACK subroutines `R_ext/Linpack.h` and `R_ext/Applic.h` declare, with R's own
//! variants (`dqrdc2`, the QR with limited pivoting behind `qr()` and `lm.fit()`, and the
//! `dqrutl` helpers that apply it), plus the PORT machine constants `d1mach` and `i1mach`.
//!
//! LINPACK is public domain; these follow the published algorithms (Dongarra, Bunch, Moler and
//! Stewart, "LINPACK Users' Guide", 1979) step for step, because callers read intermediate
//! state such as `qraux` and the pivot order, not only the solution. Every argument is a
//! pointer, as Fortran passes it, and arrays may alias where the Fortran callers alias them
//! (`dqrsl` is called with `y` and `qty` in the same storage), so the code works on raw
//! pointers with the 1-based indexing of the published algorithms.
//!
//! R's variants are not part of LINPACK. They are written from R's documentation and from
//! GNU R's observable behaviour (`tests/test_linpack.rs` holds the comparison), on top of the
//! LINPACK routines here.

// The routines index several arrays in lockstep exactly as LINPACK does, which keeps them
// checkable line by line against the published code.
#![allow(clippy::needless_range_loop)]

use std::ffi::c_int;

/// A column-major matrix with leading dimension `ld`, addressed 1-based.
#[derive(Clone, Copy)]
pub(crate) struct Matrix {
    base: *mut f64,
    ld: isize,
}

impl Matrix {
    pub(crate) fn new(base: *mut f64, ld: c_int) -> Matrix {
        Matrix {
            base,
            ld: ld as isize,
        }
    }

    /// The address of element `(row, column)`, both 1-based.
    unsafe fn at(self, row: isize, column: isize) -> *mut f64 {
        self.base.offset((row - 1) + (column - 1) * self.ld)
    }

    unsafe fn get(self, row: isize, column: isize) -> f64 {
        *self.at(row, column)
    }

    unsafe fn set(self, row: isize, column: isize, value: f64) {
        *self.at(row, column) = value;
    }
}

/// Element `index` (1-based) of a vector.
unsafe fn element(vector: *mut f64, index: isize) -> *mut f64 {
    vector.offset(index - 1)
}

// The level-1 BLAS operations LINPACK is written in, on contiguous vectors.

/// The Euclidean norm, scaled so it neither overflows nor underflows.
pub(crate) unsafe fn dnrm2(count: isize, x: *const f64) -> f64 {
    let mut scale = 0.0f64;
    let mut sum = 1.0f64;
    for index in 0..count.max(0) {
        let value = *x.offset(index);
        if value != 0.0 {
            let magnitude = value.abs();
            if scale < magnitude {
                sum = 1.0 + sum * (scale / magnitude) * (scale / magnitude);
                scale = magnitude;
            } else {
                sum += (magnitude / scale) * (magnitude / scale);
            }
        }
    }
    if scale == 0.0 {
        0.0
    } else {
        scale * sum.sqrt()
    }
}

pub(crate) unsafe fn ddot(count: isize, x: *const f64, y: *const f64) -> f64 {
    let mut sum = 0.0;
    for index in 0..count.max(0) {
        sum += *x.offset(index) * *y.offset(index);
    }
    sum
}

pub(crate) unsafe fn daxpy(count: isize, factor: f64, x: *const f64, y: *mut f64) {
    if factor == 0.0 {
        return;
    }
    for index in 0..count.max(0) {
        *y.offset(index) += factor * *x.offset(index);
    }
}

pub(crate) unsafe fn dscal(count: isize, factor: f64, x: *mut f64) {
    for index in 0..count.max(0) {
        *x.offset(index) *= factor;
    }
}

unsafe fn dasum(count: isize, x: *const f64) -> f64 {
    let mut sum = 0.0;
    for index in 0..count.max(0) {
        sum += (*x.offset(index)).abs();
    }
    sum
}

unsafe fn dswap(count: isize, x: *mut f64, y: *mut f64) {
    for index in 0..count.max(0) {
        std::ptr::swap(x.offset(index), y.offset(index));
    }
}

/// Copies element by element in increasing order, as the Fortran loop does, so overlapping
/// arguments behave as they do there.
unsafe fn dcopy(count: isize, x: *const f64, y: *mut f64) {
    for index in 0..count.max(0) {
        *y.offset(index) = *x.offset(index);
    }
}

/// Applies the plane rotation `(c, s)` to the pairs `(x[i], y[i])`.
unsafe fn drot(count: isize, x: *mut f64, y: *mut f64, c: f64, s: f64) {
    for index in 0..count.max(0) {
        let (a, b) = (*x.offset(index), *y.offset(index));
        *x.offset(index) = c * a + s * b;
        *y.offset(index) = c * b - s * a;
    }
}

/// Constructs the Givens rotation that zeroes `b` against `a`: on return `a` holds `r`, and
/// the rotation is `(c, s)`. The reconstruction value BLAS also stores in `b` is not needed.
fn drotg(a: &mut f64, b: &mut f64) -> (f64, f64) {
    let roe = if a.abs() > b.abs() { *a } else { *b };
    let scale = a.abs() + b.abs();
    if scale == 0.0 {
        *a = 0.0;
        *b = 0.0;
        return (1.0, 0.0);
    }
    let mut r = scale * ((*a / scale).powi(2) + (*b / scale).powi(2)).sqrt();
    r *= 1.0f64.copysign(roe);
    let c = *a / r;
    let s = *b / r;
    let mut z = 1.0;
    if a.abs() > b.abs() {
        z = s;
    }
    if b.abs() >= a.abs() && c != 0.0 {
        z = 1.0 / c;
    }
    *a = r;
    *b = z;
    (c, s)
}

/// Fortran's `dsign(a, b)`: the magnitude of `a` with the sign of `b`.
fn sign(a: f64, b: f64) -> f64 {
    if b >= 0.0 { a.abs() } else { -a.abs() }
}

/// R's `dqrdc2`: Householder QR with limited pivoting. A column whose norm falls below `tol`
/// times its original norm moves to the right-hand end, and `rank` counts the columns left.
/// `work` needs room for `2 * p` values.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dqrdc2_(
    x: *mut f64,
    ldx: *const c_int,
    n: *const c_int,
    p: *const c_int,
    tol: *const f64,
    rank: *mut c_int,
    qraux: *mut f64,
    pivot: *mut c_int,
    work: *mut f64,
) {
    let x = Matrix::new(x, *ldx);
    let (n, p, tol) = (*n as isize, *p as isize, *tol);
    // `qraux` carries each column's running norm below the current row until that column's
    // own reflection replaces it. `work` holds two vectors: the norm as last computed in
    // full, which callers can read back, and the original norm the tolerance is relative to.
    let refreshed_norm = |column: isize| element(work, column);
    let original_norm = |column: isize| element(work, p + column);
    let pivot_entry = |column: isize| pivot.offset(column - 1);
    for column in 1..=p {
        let norm = dnrm2(n, x.at(1, column));
        *element(qraux, column) = norm;
        *refreshed_norm(column) = norm;
        // A zero column has no scale to be relative to; measuring it against 1 still marks
        // it negligible for every positive tolerance.
        *original_norm(column) = if norm == 0.0 { 1.0 } else { norm };
    }
    // Columns after `kept` are the negligible ones already moved to the right-hand end.
    let mut kept = p;
    for l in 1..=n.min(p) {
        while l <= kept && *element(qraux, l) < *original_norm(l) * tol {
            // Rotating column `l` past every later one keeps the relative order of the kept
            // columns and of the columns moved earlier.
            for column in l..p {
                dswap(n, x.at(1, column), x.at(1, column + 1));
                std::ptr::swap(pivot_entry(column), pivot_entry(column + 1));
                std::ptr::swap(element(qraux, column), element(qraux, column + 1));
                std::ptr::swap(refreshed_norm(column), refreshed_norm(column + 1));
                std::ptr::swap(original_norm(column), original_norm(column + 1));
            }
            kept -= 1;
        }
        if l == n {
            break;
        }
        *element(qraux, l) = 0.0;
        let mut norm = dnrm2(n - l + 1, x.at(l, l));
        if norm == 0.0 {
            continue;
        }
        if x.get(l, l) != 0.0 {
            norm = sign(norm, x.get(l, l));
        }
        dscal(n - l + 1, 1.0 / norm, x.at(l, l));
        x.set(l, l, 1.0 + x.get(l, l));
        for column in l + 1..=p {
            let projection = -ddot(n - l + 1, x.at(l, l), x.at(l, column)) / x.get(l, l);
            daxpy(n - l + 1, projection, x.at(l, l), x.at(l, column));
            let running = *element(qraux, column);
            if running == 0.0 {
                continue;
            }
            let ratio = x.get(l, column).abs() / running;
            let remaining = (1.0 - ratio * ratio).max(0.0);
            // Once less than a millionth of the squared norm remains, downdating has lost
            // too many digits to decide negligibility on, so the norm is recomputed. This is
            // far earlier than LINPACK's `dqrdc` recomputes.
            if remaining < 1e-6 {
                let fresh = dnrm2(n - l, x.at(l + 1, column));
                *element(qraux, column) = fresh;
                *refreshed_norm(column) = fresh;
            } else {
                *element(qraux, column) = running * remaining.sqrt();
            }
        }
        *element(qraux, l) = x.get(l, l);
        x.set(l, l, -norm);
    }
    *rank = kept.min(n) as c_int;
}

/// LINPACK's `dqrdc`: Householder QR with optional column pivoting (`job != 0`), where
/// `jpvt[j] > 0` marks an initial column, `< 0` a final one, and `0` a free one.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dqrdc_(
    x: *mut f64,
    ldx: *const c_int,
    n: *const c_int,
    p: *const c_int,
    qraux: *mut f64,
    jpvt: *mut c_int,
    work: *mut f64,
    job: *const c_int,
) {
    let x = Matrix::new(x, *ldx);
    let (n, p) = (*n as isize, *p as isize);
    let jp = |index: isize| jpvt.offset(index - 1);
    let mut pl = 1;
    let mut pu = 0;
    if *job != 0 {
        for j in 1..=p {
            let swap = *jp(j) > 0;
            let negative = *jp(j) < 0;
            *jp(j) = if negative { -j as c_int } else { j as c_int };
            if swap {
                if j != pl {
                    dswap(n, x.at(1, pl), x.at(1, j));
                }
                *jp(j) = *jp(pl);
                *jp(pl) = j as c_int;
                pl += 1;
            }
        }
        pu = p;
        for j in (1..=p).rev() {
            if *jp(j) < 0 {
                *jp(j) = -*jp(j);
                if j != pu {
                    dswap(n, x.at(1, pu), x.at(1, j));
                    let held = *jp(pu);
                    *jp(pu) = *jp(j);
                    *jp(j) = held;
                }
                pu -= 1;
            }
        }
    }
    for j in pl..=pu {
        let norm = dnrm2(n, x.at(1, j));
        *element(qraux, j) = norm;
        *element(work, j) = norm;
    }
    let lup = n.min(p);
    for l in 1..=lup {
        if l >= pl && l < pu {
            let mut largest = 0.0;
            let mut chosen = l;
            for j in l..=pu {
                if *element(qraux, j) > largest {
                    largest = *element(qraux, j);
                    chosen = j;
                }
            }
            if chosen != l {
                dswap(n, x.at(1, l), x.at(1, chosen));
                *element(qraux, chosen) = *element(qraux, l);
                *element(work, chosen) = *element(work, l);
                let held = *jp(chosen);
                *jp(chosen) = *jp(l);
                *jp(l) = held;
            }
        }
        *element(qraux, l) = 0.0;
        if l == n {
            continue;
        }
        let mut norm = dnrm2(n - l + 1, x.at(l, l));
        if norm == 0.0 {
            continue;
        }
        if x.get(l, l) != 0.0 {
            norm = sign(norm, x.get(l, l));
        }
        dscal(n - l + 1, 1.0 / norm, x.at(l, l));
        x.set(l, l, 1.0 + x.get(l, l));
        for j in l + 1..=p {
            let t = -ddot(n - l + 1, x.at(l, l), x.at(l, j)) / x.get(l, l);
            daxpy(n - l + 1, t, x.at(l, l), x.at(l, j));
            if j < pl || j > pu || *element(qraux, j) == 0.0 {
                continue;
            }
            let ratio = x.get(l, j).abs() / *element(qraux, j);
            let t = (1.0 - ratio * ratio).max(0.0);
            let relative = *element(qraux, j) / *element(work, j);
            if 1.0 + 0.05 * t * relative * relative == 1.0 {
                let fresh = dnrm2(n - l, x.at(l + 1, j));
                *element(qraux, j) = fresh;
                *element(work, j) = fresh;
            } else {
                *element(qraux, j) *= t.sqrt();
            }
        }
        *element(qraux, l) = x.get(l, l);
        x.set(l, l, -norm);
    }
}

/// LINPACK's `dqrsl`: applies the output of `dqrdc` (or `dqrdc2`) to `y`. The decimal digits
/// of `job` select `qy`, `qty`, `b`, `rsd` and `xb`, from the ten-thousands digit down.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dqrsl_(
    x: *mut f64,
    ldx: *const c_int,
    n: *const c_int,
    k: *const c_int,
    qraux: *mut f64,
    y: *mut f64,
    qy: *mut f64,
    qty: *mut f64,
    b: *mut f64,
    rsd: *mut f64,
    xb: *mut f64,
    job: *const c_int,
    info: *mut c_int,
) {
    qr_solve(
        Matrix::new(x, *ldx),
        *n as isize,
        *k as isize,
        qraux,
        y,
        qy,
        qty,
        b,
        rsd,
        xb,
        *job,
        info,
    );
}

#[allow(clippy::too_many_arguments)]
unsafe fn qr_solve(
    x: Matrix,
    n: isize,
    k: isize,
    qraux: *mut f64,
    y: *mut f64,
    qy: *mut f64,
    qty: *mut f64,
    b: *mut f64,
    rsd: *mut f64,
    xb: *mut f64,
    job: c_int,
    info: *mut c_int,
) {
    *info = 0;
    let want_qy = job / 10000 != 0;
    let want_qty = job % 10000 != 0;
    let want_b = (job % 1000) / 100 != 0;
    let want_rsd = (job % 100) / 10 != 0;
    let want_xb = job % 10 != 0;
    let ju = k.min(n - 1);
    if ju == 0 {
        let first = *y;
        if want_qy {
            *qy = first;
        }
        if want_qty {
            *qty = first;
        }
        if want_xb {
            *xb = first;
        }
        if want_b {
            if x.get(1, 1) == 0.0 {
                *info = 1;
            } else {
                *b = first / x.get(1, 1);
            }
        }
        if want_rsd {
            *rsd = 0.0;
        }
        return;
    }
    // Applies Householder transformation j to the vector at `target`.
    let reflect = |j: isize, target: *mut f64| {
        let diagonal = x.get(j, j);
        x.set(j, j, *element(qraux, j));
        let t = -ddot(n - j + 1, x.at(j, j), element(target, j)) / x.get(j, j);
        daxpy(n - j + 1, t, x.at(j, j), element(target, j));
        x.set(j, j, diagonal);
    };
    if want_qy {
        dcopy(n, y, qy);
    }
    if want_qty {
        dcopy(n, y, qty);
    }
    if want_qy {
        for j in (1..=ju).rev() {
            if *element(qraux, j) != 0.0 {
                reflect(j, qy);
            }
        }
    }
    if want_qty {
        for j in 1..=ju {
            if *element(qraux, j) != 0.0 {
                reflect(j, qty);
            }
        }
    }
    if want_b {
        dcopy(k, qty, b);
    }
    if want_xb {
        dcopy(k, qty, xb);
    }
    if want_rsd && k < n {
        dcopy(n - k, element(qty, k + 1), element(rsd, k + 1));
    }
    if want_xb {
        for i in k + 1..=n {
            *element(xb, i) = 0.0;
        }
    }
    if want_rsd {
        for i in 1..=k {
            *element(rsd, i) = 0.0;
        }
    }
    if want_b {
        for j in (1..=k).rev() {
            if x.get(j, j) == 0.0 {
                *info = j as c_int;
                break;
            }
            *element(b, j) /= x.get(j, j);
            if j != 1 {
                daxpy(j - 1, -*element(b, j), x.at(1, j), b);
            }
        }
    }
    if want_rsd || want_xb {
        for j in (1..=ju).rev() {
            if *element(qraux, j) == 0.0 {
                continue;
            }
            if want_rsd {
                reflect(j, rsd);
            }
            if want_xb {
                reflect(j, xb);
            }
        }
    }
}

/// R's `dqrls`: the least squares fit behind `lm.fit()`. Decomposes `x` with `dqrdc2`, then
/// computes coefficients, residuals and effects for each column of `y`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dqrls_(
    x: *mut f64,
    n: *const c_int,
    p: *const c_int,
    y: *mut f64,
    ny: *const c_int,
    tol: *const f64,
    b: *mut f64,
    rsd: *mut f64,
    qty: *mut f64,
    k: *mut c_int,
    jpvt: *mut c_int,
    qraux: *mut f64,
    work: *mut f64,
) {
    dqrdc2_(x, n, n, p, tol, k, qraux, jpvt, work);
    let matrix = Matrix::new(x, *n);
    let (rows, columns, rank) = (*n as isize, *p as isize, *k as isize);
    for response in 0..(*ny as isize).max(0) {
        let observed = y.offset(response * rows);
        let residuals = rsd.offset(response * rows);
        let coefficients = b.offset(response * columns);
        if rank > 0 {
            let mut info = 0;
            qr_solve(
                matrix,
                rows,
                rank,
                qraux,
                observed,
                std::ptr::null_mut(),
                qty.offset(response * rows),
                coefficients,
                residuals,
                std::ptr::null_mut(),
                1110,
                &mut info,
            );
        } else {
            // With nothing kept, the fit is zero and every observation is a residual;
            // `qty` is left as the caller passed it.
            dcopy(rows, observed, residuals);
        }
        // The coefficients of the columns moved aside are reported as zero.
        for index in rank + 1..=columns {
            *element(coefficients, index) = 0.0;
        }
    }
}

/// Applies `qr_solve` with a single-output `job` to each of the `ny` columns of the `n`-row
/// matrix `y`, writing column by column into `output`, whose columns are `output_rows` long.
/// Jobs that compute coefficients, residuals or fitted values pass through `Q'y`, which
/// they store over `y`, as R's helpers do. Returns the last column's `info`.
#[allow(clippy::too_many_arguments)]
unsafe fn solve_each_column(
    x: *mut f64,
    n: *const c_int,
    k: *const c_int,
    qraux: *mut f64,
    y: *mut f64,
    ny: *const c_int,
    job: c_int,
    output: *mut f64,
    output_rows: isize,
) -> c_int {
    let matrix = Matrix::new(x, *n);
    let rows = *n as isize;
    let null = std::ptr::null_mut();
    let mut info = 0;
    for response in 0..(*ny as isize).max(0) {
        let observed = y.offset(response * rows);
        let target = output.offset(response * output_rows);
        let only = |wanted: c_int| if job == wanted { target } else { null };
        let transformed = if job == 1000 { target } else { observed };
        qr_solve(
            matrix,
            rows,
            *k as isize,
            qraux,
            observed,
            only(10000),
            transformed,
            only(100),
            only(10),
            only(1),
            job,
            &mut info,
        );
    }
    info
}

/// `Q'y` for each column of `y`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dqrqty_(
    x: *mut f64,
    n: *const c_int,
    k: *const c_int,
    qraux: *mut f64,
    y: *mut f64,
    ny: *const c_int,
    qty: *mut f64,
) {
    solve_each_column(x, n, k, qraux, y, ny, 1000, qty, *n as isize);
}

/// `Q y` for each column of `y`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dqrqy_(
    x: *mut f64,
    n: *const c_int,
    k: *const c_int,
    qraux: *mut f64,
    y: *mut f64,
    ny: *const c_int,
    qy: *mut f64,
) {
    solve_each_column(x, n, k, qraux, y, ny, 10000, qy, *n as isize);
}

/// The coefficients for each column of `y`, into the `k`-row matrix `b`. As in R, `y` is
/// overwritten with `Q'y`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dqrcf_(
    x: *mut f64,
    n: *const c_int,
    k: *const c_int,
    qraux: *mut f64,
    y: *mut f64,
    ny: *const c_int,
    b: *mut f64,
    info: *mut c_int,
) {
    *info = solve_each_column(x, n, k, qraux, y, ny, 100, b, *k as isize);
}

/// The residuals for each column of `y`. As in R, `y` is overwritten with `Q'y`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dqrrsd_(
    x: *mut f64,
    n: *const c_int,
    k: *const c_int,
    qraux: *mut f64,
    y: *mut f64,
    ny: *const c_int,
    rsd: *mut f64,
) {
    solve_each_column(x, n, k, qraux, y, ny, 10, rsd, *n as isize);
}

/// The fitted values for each column of `y`. As in R, `y` is overwritten with `Q'y`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dqrxb_(
    x: *mut f64,
    n: *const c_int,
    k: *const c_int,
    qraux: *mut f64,
    y: *mut f64,
    ny: *const c_int,
    xb: *mut f64,
) {
    solve_each_column(x, n, k, qraux, y, ny, 1, xb, *n as isize);
}

/// LINPACK's `dtrsl`: solves `T x = b` or `T' x = b` in place for triangular `T`. `job` is
/// 00 (lower, `T`), 01 (upper, `T`), 10 (lower, `T'`) or 11 (upper, `T'`). A zero on the
/// diagonal leaves `b` alone and sets `info` to its index.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dtrsl_(
    t: *mut f64,
    ldt: *const c_int,
    n: *const c_int,
    b: *mut f64,
    job: *const c_int,
    info: *mut c_int,
) {
    triangular_solve(Matrix::new(t, *ldt), *n as isize, b, *job, info);
}

pub(crate) unsafe fn triangular_solve(
    t: Matrix,
    n: isize,
    b: *mut f64,
    job: c_int,
    info: *mut c_int,
) {
    for index in 1..=n {
        if t.get(index, index) == 0.0 {
            *info = index as c_int;
            return;
        }
    }
    *info = 0;
    if n < 1 {
        return;
    }
    let upper = job % 10 != 0;
    let transposed = (job % 100) / 10 != 0;
    match (upper, transposed) {
        (false, false) => {
            *b /= t.get(1, 1);
            for j in 2..=n {
                daxpy(
                    n - j + 1,
                    -*element(b, j - 1),
                    t.at(j, j - 1),
                    element(b, j),
                );
                *element(b, j) /= t.get(j, j);
            }
        }
        (true, false) => {
            *element(b, n) /= t.get(n, n);
            for j in (1..n).rev() {
                daxpy(j, -*element(b, j + 1), t.at(1, j + 1), b);
                *element(b, j) /= t.get(j, j);
            }
        }
        (false, true) => {
            *element(b, n) /= t.get(n, n);
            for j in (1..n).rev() {
                *element(b, j) -= ddot(n - j, t.at(j + 1, j), element(b, j + 1));
                *element(b, j) /= t.get(j, j);
            }
        }
        (true, true) => {
            *b /= t.get(1, 1);
            for j in 2..=n {
                *element(b, j) -= ddot(j - 1, t.at(1, j), b);
                *element(b, j) /= t.get(j, j);
            }
        }
    }
}

/// LINPACK's `dtrco`: estimates the reciprocal condition number of a triangular matrix,
/// lower when `job == 0`, upper otherwise. `z` is workspace that ends up holding an
/// approximate null vector.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dtrco_(
    t: *mut f64,
    ldt: *const c_int,
    n: *const c_int,
    rcond: *mut f64,
    z: *mut f64,
    job: *const c_int,
) {
    let t = Matrix::new(t, *ldt);
    let n = *n as isize;
    let lower = *job == 0;
    let mut tnorm = 0.0f64;
    for j in 1..=n {
        let (length, start) = if lower { (n + 1 - j, j) } else { (j, 1) };
        tnorm = tnorm.max(dasum(length, t.at(start, j)));
    }
    // Solve trans(t) y = e, choosing the signs of e to make y large.
    let mut ek = 1.0f64;
    for j in 1..=n {
        *element(z, j) = 0.0;
    }
    for kk in 1..=n {
        let k = if lower { n + 1 - kk } else { kk };
        let zk = *element(z, k);
        if zk != 0.0 {
            ek = sign(ek, -zk);
        }
        if (ek - zk).abs() > t.get(k, k).abs() {
            let s = t.get(k, k).abs() / (ek - zk).abs();
            dscal(n, s, z);
            ek *= s;
        }
        let zk = *element(z, k);
        let mut wk = ek - zk;
        let mut wkm = -ek - zk;
        let mut s = wk.abs();
        let mut sm = wkm.abs();
        if t.get(k, k) != 0.0 {
            wk /= t.get(k, k);
            wkm /= t.get(k, k);
        } else {
            wk = 1.0;
            wkm = 1.0;
        }
        if kk != n {
            let (j1, j2) = if lower { (1, k - 1) } else { (k + 1, n) };
            for j in j1..=j2 {
                sm += (*element(z, j) + wkm * t.get(k, j)).abs();
                *element(z, j) += wk * t.get(k, j);
                s += (*element(z, j)).abs();
            }
            if s < sm {
                let w = wkm - wk;
                wk = wkm;
                for j in j1..=j2 {
                    *element(z, j) += w * t.get(k, j);
                }
            }
        }
        *element(z, k) = wk;
    }
    let s = 1.0 / dasum(n, z);
    dscal(n, s, z);
    let mut ynorm = 1.0;
    // Solve t z = y.
    for kk in 1..=n {
        let k = if lower { kk } else { n + 1 - kk };
        if (*element(z, k)).abs() > t.get(k, k).abs() {
            let s = t.get(k, k).abs() / (*element(z, k)).abs();
            dscal(n, s, z);
            ynorm *= s;
        }
        if t.get(k, k) != 0.0 {
            *element(z, k) /= t.get(k, k);
        } else {
            *element(z, k) = 1.0;
        }
        let i1 = if lower { k + 1 } else { 1 };
        if kk < n {
            let w = -*element(z, k);
            daxpy(n - kk, w, t.at(i1, k), element(z, i1));
        }
    }
    let s = 1.0 / dasum(n, z);
    dscal(n, s, z);
    ynorm *= s;
    *rcond = if tnorm != 0.0 { ynorm / tnorm } else { 0.0 };
}

/// LINPACK's `dpofa`: the Cholesky factor `R` (upper, `A = R'R`) of a symmetric positive
/// definite matrix, in place. `info` is the order of the leading minor that is not positive
/// definite, or 0. Like R's copy, a pivot below a relative 1e-14 counts as not positive.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dpofa_(a: *mut f64, lda: *const c_int, n: *const c_int, info: *mut c_int) {
    let a = Matrix::new(a, *lda);
    let n = *n as isize;
    const TOLERANCE: f64 = 1e-14;
    for j in 1..=n {
        *info = j as c_int;
        let mut s = 0.0;
        for k in 1..j {
            let t = (a.get(k, j) - ddot(k - 1, a.at(1, k), a.at(1, j))) / a.get(k, k);
            a.set(k, j, t);
            s += t * t;
        }
        let s = a.get(j, j) - s;
        if s <= TOLERANCE * a.get(j, j).abs() {
            return;
        }
        a.set(j, j, s.sqrt());
    }
    *info = 0;
}

/// LINPACK's `dposl`: solves `A x = b` in place from the factor `dpofa` produced.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dposl_(a: *mut f64, lda: *const c_int, n: *const c_int, b: *mut f64) {
    let a = Matrix::new(a, *lda);
    let n = *n as isize;
    for k in 1..=n {
        let t = ddot(k - 1, a.at(1, k), b);
        *element(b, k) = (*element(b, k) - t) / a.get(k, k);
    }
    for k in (1..=n).rev() {
        *element(b, k) /= a.get(k, k);
        daxpy(k - 1, -*element(b, k), a.at(1, k), b);
    }
}

/// LINPACK's `dpoco`: factors with `dpofa` and estimates the reciprocal condition number.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dpoco_(
    a: *mut f64,
    lda: *const c_int,
    n: *const c_int,
    rcond: *mut f64,
    z: *mut f64,
    info: *mut c_int,
) {
    let matrix = Matrix::new(a, *lda);
    let size = *n as isize;
    for j in 1..=size {
        *element(z, j) = dasum(j, matrix.at(1, j));
        for i in 1..j {
            *element(z, i) += matrix.get(i, j).abs();
        }
    }
    let mut anorm = 0.0f64;
    for j in 1..=size {
        anorm = anorm.max(*element(z, j));
    }
    dpofa_(a, lda, n, info);
    if *info != 0 {
        return;
    }
    let a = matrix;
    let n = size;
    // Solve trans(r) w = e.
    let mut ek = 1.0f64;
    for j in 1..=n {
        *element(z, j) = 0.0;
    }
    for k in 1..=n {
        if *element(z, k) != 0.0 {
            ek = sign(ek, -*element(z, k));
        }
        if (ek - *element(z, k)).abs() > a.get(k, k) {
            let s = a.get(k, k) / (ek - *element(z, k)).abs();
            dscal(n, s, z);
            ek *= s;
        }
        let mut wk = ek - *element(z, k);
        let mut wkm = -ek - *element(z, k);
        let mut s = wk.abs();
        let mut sm = wkm.abs();
        wk /= a.get(k, k);
        wkm /= a.get(k, k);
        if k < n {
            for j in k + 1..=n {
                sm += (*element(z, j) + wkm * a.get(k, j)).abs();
                *element(z, j) += wk * a.get(k, j);
                s += (*element(z, j)).abs();
            }
            if s < sm {
                let t = wkm - wk;
                wk = wkm;
                for j in k + 1..=n {
                    *element(z, j) += t * a.get(k, j);
                }
            }
        }
        *element(z, k) = wk;
    }
    let s = 1.0 / dasum(n, z);
    dscal(n, s, z);
    // Solve r y = w.
    for k in (1..=n).rev() {
        if (*element(z, k)).abs() > a.get(k, k) {
            let s = a.get(k, k) / (*element(z, k)).abs();
            dscal(n, s, z);
        }
        *element(z, k) /= a.get(k, k);
        daxpy(k - 1, -*element(z, k), a.at(1, k), z);
    }
    let s = 1.0 / dasum(n, z);
    dscal(n, s, z);
    let mut ynorm = 1.0;
    // Solve trans(r) v = y.
    for k in 1..=n {
        *element(z, k) -= ddot(k - 1, a.at(1, k), z);
        if (*element(z, k)).abs() > a.get(k, k) {
            let s = a.get(k, k) / (*element(z, k)).abs();
            dscal(n, s, z);
            ynorm *= s;
        }
        *element(z, k) /= a.get(k, k);
    }
    let s = 1.0 / dasum(n, z);
    dscal(n, s, z);
    ynorm *= s;
    // Solve r z = v.
    for k in (1..=n).rev() {
        if (*element(z, k)).abs() > a.get(k, k) {
            let s = a.get(k, k) / (*element(z, k)).abs();
            dscal(n, s, z);
            ynorm *= s;
        }
        *element(z, k) /= a.get(k, k);
        daxpy(k - 1, -*element(z, k), a.at(1, k), z);
    }
    let s = 1.0 / dasum(n, z);
    dscal(n, s, z);
    ynorm *= s;
    *rcond = if anorm != 0.0 { ynorm / anorm } else { 0.0 };
}

/// LINPACK's `dpodi`: from the factor `dpofa` produced, the determinant as
/// `det[0] * 10^det[1]` (tens digit of `job`) and the inverse in the upper triangle (units
/// digit).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dpodi_(
    a: *mut f64,
    lda: *const c_int,
    n: *const c_int,
    det: *mut f64,
    job: *const c_int,
) {
    let a = Matrix::new(a, *lda);
    let n = *n as isize;
    if *job / 10 != 0 {
        let (mut mantissa, mut exponent) = (1.0f64, 0.0f64);
        for i in 1..=n {
            mantissa *= a.get(i, i) * a.get(i, i);
            // An overflowed diagonal never scales back below 10.
            if mantissa == 0.0 || !mantissa.is_finite() {
                break;
            }
            while mantissa < 1.0 {
                mantissa *= 10.0;
                exponent -= 1.0;
            }
            while mantissa >= 10.0 {
                mantissa /= 10.0;
                exponent += 1.0;
            }
        }
        *det = mantissa;
        *det.offset(1) = exponent;
    }
    if *job % 10 == 0 {
        return;
    }
    for k in 1..=n {
        a.set(k, k, 1.0 / a.get(k, k));
        let t = -a.get(k, k);
        dscal(k - 1, t, a.at(1, k));
        for j in k + 1..=n {
            let t = a.get(k, j);
            a.set(k, j, 0.0);
            daxpy(k, t, a.at(1, k), a.at(1, j));
        }
    }
    for j in 1..=n {
        for k in 1..j {
            let t = a.get(k, j);
            daxpy(k, t, a.at(1, j), a.at(1, k));
        }
        let t = a.get(j, j);
        dscal(j, t, a.at(1, j));
    }
}

/// LINPACK's `dpbfa`: the Cholesky factor of a positive definite band matrix with `m`
/// superdiagonals, stored in LINPACK's band layout.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dpbfa_(
    abd: *mut f64,
    lda: *const c_int,
    n: *const c_int,
    m: *const c_int,
    info: *mut c_int,
) {
    let abd = Matrix::new(abd, *lda);
    let (n, m) = (*n as isize, *m as isize);
    for j in 1..=n {
        *info = j as c_int;
        let mut s = 0.0;
        let first_column = (j - m).max(1);
        let mu = (m + 2 - j).max(1);
        for (offset, k) in (mu..=m).enumerate() {
            let (ik, jk) = (m + 1 - offset as isize, first_column + offset as isize);
            let t =
                (abd.get(k, j) - ddot(k - mu, abd.at(ik, jk), abd.at(mu, j))) / abd.get(m + 1, jk);
            abd.set(k, j, t);
            s += t * t;
        }
        let s = abd.get(m + 1, j) - s;
        if s <= 0.0 {
            return;
        }
        abd.set(m + 1, j, s.sqrt());
    }
    *info = 0;
}

/// LINPACK's `dpbsl`: solves with the band factor `dpbfa` produced.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dpbsl_(
    abd: *mut f64,
    lda: *const c_int,
    n: *const c_int,
    m: *const c_int,
    b: *mut f64,
) {
    let abd = Matrix::new(abd, *lda);
    let (n, m) = (*n as isize, *m as isize);
    for k in 1..=n {
        let lm = (k - 1).min(m);
        let la = m + 1 - lm;
        let lb = k - lm;
        let t = ddot(lm, abd.at(la, k), element(b, lb));
        *element(b, k) = (*element(b, k) - t) / abd.get(m + 1, k);
    }
    for k in (1..=n).rev() {
        let lm = (k - 1).min(m);
        let la = m + 1 - lm;
        let lb = k - lm;
        *element(b, k) /= abd.get(m + 1, k);
        daxpy(lm, -*element(b, k), abd.at(la, k), element(b, lb));
    }
}

/// LINPACK's `dsvdc`: the singular value decomposition `X = U S V'` by Householder
/// bidiagonalization and implicitly shifted QR. `job` is two decimal digits: the tens digit
/// asks for `U` (1 for all `n` columns, 2 or more for the first `min(n, p)`), the units digit
/// for `V`. `info` is nonzero when 30 iterations did not converge for some value.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dsvdc_(
    x: *mut f64,
    ldx: *const c_int,
    n: *const c_int,
    p: *const c_int,
    s: *mut f64,
    e: *mut f64,
    u: *mut f64,
    ldu: *const c_int,
    v: *mut f64,
    ldv: *const c_int,
    work: *mut f64,
    job: *const c_int,
    info: *mut c_int,
) {
    let x = Matrix::new(x, *ldx);
    let u = Matrix::new(u, *ldu);
    let v = Matrix::new(v, *ldv);
    let (n, p) = (*n as isize, *p as isize);
    let s_at = |index: isize| element(s, index);
    let e_at = |index: isize| element(e, index);
    const MAX_ITERATIONS: isize = 30;
    let job_u = (*job % 100) / 10;
    let ncu = if job_u > 1 { n.min(p) } else { n };
    let want_u = job_u != 0;
    let want_v = *job % 10 != 0;
    *info = 0;
    let nct = (n - 1).min(p);
    let nrt = (p - 2).min(n).max(0);
    let lu = nct.max(nrt);
    for l in 1..=lu {
        if l <= nct {
            *s_at(l) = dnrm2(n - l + 1, x.at(l, l));
            if *s_at(l) != 0.0 {
                if x.get(l, l) != 0.0 {
                    *s_at(l) = sign(*s_at(l), x.get(l, l));
                }
                dscal(n - l + 1, 1.0 / *s_at(l), x.at(l, l));
                x.set(l, l, 1.0 + x.get(l, l));
            }
            *s_at(l) = -*s_at(l);
        }
        for j in l + 1..=p {
            if l <= nct && *s_at(l) != 0.0 {
                let t = -ddot(n - l + 1, x.at(l, l), x.at(l, j)) / x.get(l, l);
                daxpy(n - l + 1, t, x.at(l, l), x.at(l, j));
            }
            *e_at(j) = x.get(l, j);
        }
        if want_u && l <= nct {
            for i in l..=n {
                u.set(i, l, x.get(i, l));
            }
        }
        if l <= nrt {
            *e_at(l) = dnrm2(p - l, e_at(l + 1));
            if *e_at(l) != 0.0 {
                if *e_at(l + 1) != 0.0 {
                    *e_at(l) = sign(*e_at(l), *e_at(l + 1));
                }
                dscal(p - l, 1.0 / *e_at(l), e_at(l + 1));
                *e_at(l + 1) += 1.0;
            }
            *e_at(l) = -*e_at(l);
            if l < n && *e_at(l) != 0.0 {
                for i in l + 1..=n {
                    *element(work, i) = 0.0;
                }
                for j in l + 1..=p {
                    daxpy(n - l, *e_at(j), x.at(l + 1, j), element(work, l + 1));
                }
                for j in l + 1..=p {
                    daxpy(
                        n - l,
                        -*e_at(j) / *e_at(l + 1),
                        element(work, l + 1),
                        x.at(l + 1, j),
                    );
                }
            }
            if want_v {
                for i in l + 1..=p {
                    v.set(i, l, *e_at(i));
                }
            }
        }
    }
    let mut m = p.min(n + 1);
    let nctp1 = nct + 1;
    let nrtp1 = nrt + 1;
    if nct < p {
        *s_at(nctp1) = x.get(nctp1, nctp1);
    }
    if n < m {
        *s_at(m) = 0.0;
    }
    if nrtp1 < m {
        *e_at(nrtp1) = x.get(nrtp1, m);
    }
    *e_at(m) = 0.0;
    if want_u {
        for j in nctp1..=ncu {
            for i in 1..=n {
                u.set(i, j, 0.0);
            }
            u.set(j, j, 1.0);
        }
        for l in (1..=nct).rev() {
            if *s_at(l) != 0.0 {
                for j in l + 1..=ncu {
                    let t = -ddot(n - l + 1, u.at(l, l), u.at(l, j)) / u.get(l, l);
                    daxpy(n - l + 1, t, u.at(l, l), u.at(l, j));
                }
                dscal(n - l + 1, -1.0, u.at(l, l));
                u.set(l, l, 1.0 + u.get(l, l));
                for i in 1..l {
                    u.set(i, l, 0.0);
                }
            } else {
                for i in 1..=n {
                    u.set(i, l, 0.0);
                }
                u.set(l, l, 1.0);
            }
        }
    }
    if want_v {
        for l in (1..=p).rev() {
            if l <= nrt && *e_at(l) != 0.0 {
                for j in l + 1..=p {
                    let t = -ddot(p - l, v.at(l + 1, l), v.at(l + 1, j)) / v.get(l + 1, l);
                    daxpy(p - l, t, v.at(l + 1, l), v.at(l + 1, j));
                }
            }
            for i in 1..=p {
                v.set(i, l, 0.0);
            }
            v.set(l, l, 1.0);
        }
    }
    let mm = m;
    let mut iteration = 0;
    while m != 0 {
        if iteration >= MAX_ITERATIONS {
            *info = m as c_int;
            return;
        }
        // Find the negligible elements; `kase` says what to do with the trailing block.
        let mut l = m - 1;
        while l > 0 {
            let test = (*s_at(l)).abs() + (*s_at(l + 1)).abs();
            if test + (*e_at(l)).abs() == test {
                *e_at(l) = 0.0;
                break;
            }
            l -= 1;
        }
        let kase;
        if l == m - 1 {
            kase = 4;
        } else {
            let mut ls = m;
            while ls > l {
                let mut test = 0.0;
                if ls != m {
                    test += (*e_at(ls)).abs();
                }
                if ls != l + 1 {
                    test += (*e_at(ls - 1)).abs();
                }
                if test + (*s_at(ls)).abs() == test {
                    *s_at(ls) = 0.0;
                    break;
                }
                ls -= 1;
            }
            if ls == l {
                kase = 3;
            } else if ls == m {
                kase = 1;
            } else {
                kase = 2;
                l = ls;
            }
        }
        l += 1;
        match kase {
            1 => {
                // Deflate a negligible s(m).
                let mut f = *e_at(m - 1);
                *e_at(m - 1) = 0.0;
                for k in (l..=m - 1).rev() {
                    let mut t1 = *s_at(k);
                    let (cs, sn) = drotg(&mut t1, &mut f);
                    *s_at(k) = t1;
                    if k != l {
                        f = -sn * *e_at(k - 1);
                        *e_at(k - 1) *= cs;
                    }
                    if want_v {
                        drot(p, v.at(1, k), v.at(1, m), cs, sn);
                    }
                }
            }
            2 => {
                // Split at a negligible s(l).
                let mut f = *e_at(l - 1);
                *e_at(l - 1) = 0.0;
                for k in l..=m {
                    let mut t1 = *s_at(k);
                    let (cs, sn) = drotg(&mut t1, &mut f);
                    *s_at(k) = t1;
                    f = -sn * *e_at(k);
                    *e_at(k) *= cs;
                    if want_u {
                        drot(n, u.at(1, k), u.at(1, l - 1), cs, sn);
                    }
                }
            }
            3 => {
                // One QR step, with the shift from the trailing 2x2 block.
                let scale = (*s_at(m))
                    .abs()
                    .max((*s_at(m - 1)).abs())
                    .max((*e_at(m - 1)).abs())
                    .max((*s_at(l)).abs())
                    .max((*e_at(l)).abs());
                let sm = *s_at(m) / scale;
                let smm1 = *s_at(m - 1) / scale;
                let emm1 = *e_at(m - 1) / scale;
                let sl = *s_at(l) / scale;
                let el = *e_at(l) / scale;
                let b = ((smm1 + sm) * (smm1 - sm) + emm1 * emm1) / 2.0;
                let c = (sm * emm1) * (sm * emm1);
                let mut shift = 0.0;
                if b != 0.0 || c != 0.0 {
                    shift = (b * b + c).sqrt();
                    if b < 0.0 {
                        shift = -shift;
                    }
                    shift = c / (b + shift);
                }
                let mut f = (sl + sm) * (sl - sm) + shift;
                let mut g = sl * el;
                for k in l..=m - 1 {
                    let (cs, sn) = drotg(&mut f, &mut g);
                    if k != l {
                        *e_at(k - 1) = f;
                    }
                    f = cs * *s_at(k) + sn * *e_at(k);
                    *e_at(k) = cs * *e_at(k) - sn * *s_at(k);
                    g = sn * *s_at(k + 1);
                    *s_at(k + 1) *= cs;
                    if want_v {
                        drot(p, v.at(1, k), v.at(1, k + 1), cs, sn);
                    }
                    let (cs, sn) = drotg(&mut f, &mut g);
                    *s_at(k) = f;
                    f = cs * *e_at(k) + sn * *s_at(k + 1);
                    *s_at(k + 1) = -sn * *e_at(k) + cs * *s_at(k + 1);
                    g = sn * *e_at(k + 1);
                    *e_at(k + 1) *= cs;
                    if want_u && k < n {
                        drot(n, u.at(1, k), u.at(1, k + 1), cs, sn);
                    }
                }
                *e_at(m - 1) = f;
                iteration += 1;
            }
            _ => {
                // Convergence: make the value positive and move it into order.
                if *s_at(l) < 0.0 {
                    *s_at(l) = -*s_at(l);
                    if want_v {
                        dscal(p, -1.0, v.at(1, l));
                    }
                }
                while l != mm && *s_at(l) < *s_at(l + 1) {
                    let t = *s_at(l);
                    *s_at(l) = *s_at(l + 1);
                    *s_at(l + 1) = t;
                    if want_v && l < p {
                        dswap(p, v.at(1, l), v.at(1, l + 1));
                    }
                    if want_u && l < n {
                        dswap(n, u.at(1, l), u.at(1, l + 1));
                    }
                    l += 1;
                }
                iteration = 0;
                m -= 1;
            }
        }
    }
}

/// PORT's `d1mach`: 1 the smallest positive normal double, 2 the largest, 3 the smallest
/// relative spacing, 4 the largest relative spacing (machine epsilon), 5 `log10(2)`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn d1mach_(which: *const c_int) -> f64 {
    match *which {
        1 => f64::MIN_POSITIVE,
        2 => f64::MAX,
        3 => f64::EPSILON / 2.0,
        4 => f64::EPSILON,
        5 => std::f64::consts::LOG10_2,
        _ => 0.0,
    }
}

/// PORT's `i1mach`: I/O units, integer and floating-point parameters of the machine.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn i1mach_(which: *const c_int) -> c_int {
    match *which {
        1 => 5,
        2 => 6,
        3 => 0,
        4 => 0,
        5 => c_int::BITS as c_int,
        6 => std::mem::size_of::<c_int>() as c_int,
        7 => 2,
        8 => c_int::BITS as c_int - 1,
        9 => c_int::MAX,
        10 => f64::RADIX as c_int,
        11 => f32::MANTISSA_DIGITS as c_int,
        12 => f32::MIN_EXP,
        13 => f32::MAX_EXP,
        14 => f64::MANTISSA_DIGITS as c_int,
        15 => f64::MIN_EXP,
        16 => f64::MAX_EXP,
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dqrdc2_solves_a_full_rank_system() {
        // Columns (1, 1, 1) and (1, 2, 3); y = 2 + 3 t is fitted exactly.
        let mut x = [1.0, 1.0, 1.0, 1.0, 2.0, 3.0];
        let mut y = [5.0, 8.0, 11.0];
        let (n, p, ny) = (3, 2, 1);
        let tol = 1e-7;
        let mut b = [0.0; 2];
        let mut rsd = [0.0; 3];
        let mut qty = [0.0; 3];
        let mut k = 0;
        let mut jpvt = [1, 2];
        let mut qraux = [0.0; 2];
        let mut work = [0.0; 4];
        unsafe {
            dqrls_(
                x.as_mut_ptr(),
                &n,
                &p,
                y.as_mut_ptr(),
                &ny,
                &tol,
                b.as_mut_ptr(),
                rsd.as_mut_ptr(),
                qty.as_mut_ptr(),
                &mut k,
                jpvt.as_mut_ptr(),
                qraux.as_mut_ptr(),
                work.as_mut_ptr(),
            );
        }
        assert_eq!(k, 2);
        assert!(
            (b[0] - 2.0).abs() < 1e-12 && (b[1] - 3.0).abs() < 1e-12,
            "{b:?}"
        );
        assert!(rsd.iter().all(|value| value.abs() < 1e-12), "{rsd:?}");
    }
}
