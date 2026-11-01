//! Base R's linear algebra, pinned to GNU R 4.3.3's results bit for bit: the matrix products
//! (BLAS), the `La_*` internals (LAPACK), `backsolve`, and the LINPACK routines base registers
//! for `.Fortran`, `dqrdc2` among them. The expected values are GNU R's on the reference BLAS
//! and LAPACK 3.12, the libraries both run on.
//! A machine whose default BLAS or LAPACK is OpenBLAS differs in the last bit, so run the test with
//! `LD_LIBRARY_PATH=/usr/lib/x86_64-linux-gnu/blas:/usr/lib/x86_64-linux-gnu/lapack` there.
//!
//! The interpreter runs here without base R and without libR, so the cases build their inputs
//! from primitives and call `.Internal` and `.Fortran` directly. That also checks that base's
//! `.Fortran` routines work without the libR host.

use {
    heap::{
        Heap,
        object::{Sexp, SexpType},
    },
    interp::{eval, lower, unwind::Unwind, values},
};

/// The inputs: a 9 x 3 matrix `x`, a 9 x 5 matrix `n` whose second column nearly repeats its
/// first (so `dqrdc2` must recompute that column's norm) and whose fourth is the sum of two
/// others (so it is pivoted to the end), a square `a`, a positive definite `s`, the `dqrdc2`
/// decomposition `q` of `n`, `chol(s)` as `r`, and LAPACK's QR of `x` as `lq`.
const SETUP: &str = r#"
k <- 1:27
x <- sin(k * 1.7) * 10 + cos(k * k * 0.3)
dim(x) <- c(9L, 3L)
n <- c(x[1:9], x[1:9] + 1e-5 * sin(1:9), x[10:18], x[1:9] + x[10:18], x[19:27])
dim(n) <- c(9L, 5L)
a <- x[1:9]
dim(a) <- c(3L, 3L)
s <- .Internal(crossprod(a, NULL)) + c(1, 0, 0, 0, 1, 0, 0, 0, 1)
b <- c(1, 2, 3)
y <- sin(1:9) * 3 + 1:9
ym <- y
dim(ym) <- c(9L, 1L)
q <- .Fortran("dqrdc2", qr = n, 9L, 9L, 5L, 1e-7, rank = 0L, qraux = rep(0, 5), pivot = 1:5, rep(0, 10), PACKAGE = "base")
r <- .Internal(La_chol(s, FALSE, -1))
u <- rep(0, 27)
dim(u) <- c(9L, 3L)
v <- rep(0, 9)
dim(v) <- c(3L, 3L)
z <- a + a * 1i
lq <- .Internal(La_qr(x))
w <- u[1] + 0i
dim(w) <- c(1L, 1L)
singular <- c(1, 2, 2, 4)
dim(singular) <- c(2L, 2L)
nearly_singular <- c(1, 2, 2, 4 + 1e-15)
dim(nearly_singular) <- c(2L, 2L)
zero_diagonal <- c(1, 0, 2, 0)
dim(zero_diagonal) <- c(2L, 2L)
"#;

/// Each case's name, expression, and the bits of GNU R's result as doubles.
const CASES: &[(&str, &str, &[u64])] = &[
    (
        "matprod_vector",
        r#"x %*% c(1, 2, 3)"#,
        &[
            0x402ed05631f15092,
            0x402af7a42980dd2d,
            0xc02a721afa9e745e,
            0xbffd9a32a634e5e4,
            0x40310e7527637f42,
            0xbffe11c314e4f604,
            0xc0310b26705ee4f6,
            0x40119abb0877295c,
            0x40316065a91feffa,
        ],
    ),
    (
        "matprod",
        r#"a %*% a"#,
        &[
            0x4065ed25966418e9,
            0xc0615efff88c45b9,
            0xc061bba5c679aa85,
            0x40621bcaaaba6873,
            0xc0245718f8716af8,
            0xc0620d161b7106ec,
            0xc04c1e5376672000,
            0x40614b74f97286b4,
            0x403536fb4bf303b7,
        ],
    ),
    (
        "crossprod",
        r#".Internal(crossprod(x, NULL))"#,
        &[
            0x408091731e96e670,
            0xc07d8d9f14a97360,
            0x407650ecf315d00d,
            0xc07d8d9f14a97360,
            0x407f905c6835e202,
            0xc07c0a4ae3f28dcd,
            0x407650ecf315d00d,
            0xc07c0a4ae3f28dcd,
            0x407c605e9c50cd83,
        ],
    ),
    (
        "tcrossprod",
        r#".Internal(tcrossprod(a, s))"#,
        &[
            0x40af01aa98328227,
            0xc08d14c65db7e91c,
            0xc0ad4fb71b6a163e,
            0x409ce630fec9dacd,
            0x4091448dabdf2442,
            0xc0a088cbda65544a,
            0xc0a3d0061de511d3,
            0x409e070ed4325776,
            0x40a0413129514d58,
        ],
    ),
    (
        "solve",
        r#".Internal(La_solve(a, b, 2.220446e-16))"#,
        &[0x4020f2595635930f, 0xc01c1f0b84700619, 0x40209c447e0b9869],
    ),
    (
        "dqrdc2_qr",
        r#"q$qr"#,
        &[
            0xc0370691ed673e2c,
            0xbfb861e6e83a9139,
            0xbfdc3ef74dead30f,
            0x3fcbf4491edd2cb7,
            0x3fd72850c3e8e286,
            0xbfd3fe42f05e8bda,
            0xbfd2aa0d6d2e2788,
            0x3fda7db46f9ee852,
            0x3fc9cb958f3cfd0e,
            0xc03706920247c09a,
            0xbef6b556fbfc99c8,
            0x3fc974ba337a07c7,
            0xbfda99983071cfe7,
            0xbfe1ad608ababf08,
            0xbfa199fa45145057,
            0x3fd911f955630128,
            0x3fd5364f7de9c8e5,
            0x3fc08b0f57c72c7a,
            0x4034892cc2ccc253,
            0xbfdc553b1bda9080,
            0xc0223b842ae7c4bd,
            0x3fd84e8be5a9c136,
            0xbfdc6cdb2f1fde52,
            0xbfd441fec0c0cac0,
            0x3fda8ac06f05964c,
            0x3fd8bc1bcd7e7a9e,
            0xbfdb70270081adbe,
            0xc02f038f19196c8e,
            0xbfe2a5a157d7fa48,
            0x402c9eb21260a416,
            0xc0073eef84c9fccd,
            0x3fda9666cb2ed95d,
            0x3fb8be5939084f5e,
            0xbfdf397489ecec1c,
            0xbfdb016447b49868,
            0x3f90dfbc817a070a,
            0xc003eb2954d3ded0,
            0xbfdc553b1bda9080,
            0xc0223b842ae7c4be,
            0xbcc696696b9cff48,
            0xbcf0bbea30715e17,
            0x3fdfc47d0ce922a5,
            0xbfd535c9b9dbd143,
            0xbfe1dacf52dce13a,
            0x3fdecbc632c2f0f3,
        ],
    ),
    ("dqrdc2_rank", r#"q$rank"#, &[0x4010000000000000]),
    (
        "dqrdc2_qraux",
        r#"q$qraux"#,
        &[
            0x3ff78e0028eee54f,
            0x3ff72e00338c3751,
            0x3ff3b03bb813ecd3,
            0x3ffa241ea73fcc0a,
            0x3ff5154232e36953,
        ],
    ),
    (
        "dqrdc2_pivot",
        r#"q$pivot"#,
        &[
            0x3ff0000000000000,
            0x4000000000000000,
            0x4008000000000000,
            0x4014000000000000,
            0x4010000000000000,
        ],
    ),
    (
        "dqrcf",
        r#".Fortran("dqrcf", q$qr, 9L, q$rank, q$qraux, y, 1L, coef = rep(0, q$rank), info = 0L, PACKAGE = "base")$coef"#,
        &[
            0xc11bebb18da3b685,
            0x411bebb3dbc87505,
            0x3ff1d79b5c774a45,
            0x3fe959757f12a687,
        ],
    ),
    (
        "dqrqty",
        r#".Fortran("dqrqty", q$qr, 9L, q$rank, q$qraux, y, 1L, qty = y, PACKAGE = "base")$qty"#,
        &[
            0xc009c8ce0fdec0d0,
            0xc025b9600a37bc5a,
            0x3ff2b882dfd6f638,
            0xc0026a2784522fc0,
            0x401631a839231de4,
            0x401c60cd1631f77d,
            0x401d71bb8a6435d4,
            0x401a3a5593b607d8,
            0x40211253430442fc,
        ],
    ),
    (
        "dqrqy",
        r#".Fortran("dqrqy", q$qr, 9L, q$rank, q$qraux, y, 1L, qy = y, PACKAGE = "base")$qy"#,
        &[
            0xc0234ca82659bbbc,
            0xc0004ef25afb30ac,
            0x3fd7b25bf47c06f8,
            0x401301e57a7d8f70,
            0x40147845ee99d6e0,
            0x4023e36f4b629f6e,
            0x401885d2f49ca520,
            0x4003a2bdc3a46809,
            0x4023c0277663843a,
        ],
    ),
    (
        "dqrrsd",
        r#".Fortran("dqrrsd", q$qr, 9L, q$rank, q$qraux, y, 1L, rsd = y, PACKAGE = "base")$rsd"#,
        &[
            0xc001e1331b5741be,
            0xbff887314ef16492,
            0x400941eb8b9bd56a,
            0x4010dc59204500dc,
            0x4015e9f6ce5de85e,
            0x401c66e3532f61fb,
            0x401d35c340ecf19a,
            0x40164278f4a81ac6,
            0x401ca9f06b720a16,
        ],
    ),
    (
        "dqrxb",
        r#".Fortran("dqrxb", q$qr, 9L, q$rank, q$qraux, y, 1L, xb = y, PACKAGE = "base")$xb"#,
        &[
            0x40170999434f05be,
            0x40190b28ebcafcc1,
            0x3fd108f847b394b0,
            0xc003e27da6ef18c2,
            0xc00ad78f152443a4,
            0xbfff050274c20206,
            0x3ffab2001fefc8ac,
            0x40159cd60767330c,
            0x4008902d7d607dae,
        ],
    ),
    (
        "dtrco",
        r#".Fortran("dtrco", r, 3L, 3L, k = 0, rep(0, 3), 1L, PACKAGE = "base")$k"#,
        &[0x3fa1b45908275139],
    ),
    (
        "chol",
        r#"r"#,
        &[
            0x402e26d1587078b0,
            0x0000000000000000,
            0x0000000000000000,
            0x401d0d6e436c9bdc,
            0x4023762c9ad7a927,
            0x0000000000000000,
            0xc022b6b2425f83b2,
            0x40207cf34247b2e7,
            0x3ffb752297c81f1e,
        ],
    ),
    (
        "chol2inv",
        r#".Internal(La_chol2inv(r, 3L))"#,
        &[
            0x3fd770d28152a32e,
            0xbfd3447f6c5bd8f7,
            0x3fd65ba678a77d3e,
            0xbfd3447f6c5bd8f7,
            0x3fd046355c16f5b6,
            0xbfd269532760d716,
            0x3fd65ba678a77d3e,
            0xbfd269532760d716,
            0x3fd5bb58828dc732,
        ],
    ),
    (
        "backsolve",
        r#".Internal(backsolve(r, b, 3L, TRUE, FALSE))"#,
        &[0x3ffc40dd0b48963c, 0xbff4688805c3b6e2, 0x3ffbf86ebbda6c4a],
    ),
    (
        "la_qr",
        r#"lq$qr"#,
        &[
            0xc0370691ed673e2d,
            0xbfb08ff033870af7,
            0xbfd32fc7b272e5d5,
            0x3fc2fd0d489ef7e9,
            0x3fcf75db4be15c54,
            0xbfcb295d8e8fec73,
            0xbfc95b2d7e6c5bce,
            0x3fd1fe9c3c833d0d,
            0x3fc1859e46c0f659,
            0xc02f038f19196c8c,
            0xc02d3a2af2142c09,
            0xbfaad9f5aabb687d,
            0xbfd03888eb169ebc,
            0x3fc7752cfa20d0a0,
            0x3fc9167a16bc614a,
            0xbfcb9ea0994ce7d8,
            0xbfca4974656a3b0e,
            0x3fd2e58b09590f71,
            0x4034892cc2ccc253,
            0x4021d17dda7bbfcc,
            0xbfffb6a2b1c524f2,
            0x3fd8d22c4fe884bd,
            0xbfcf2956ba733356,
            0xbfc2254370fe98a6,
            0x3fc07432f7a70b7e,
            0x3fbfc71cf3eda51d,
            0xbfc7b65bcc9ac630,
        ],
    ),
    (
        "la_qr_qraux",
        r#"lq$qraux"#,
        &[0x3ff78e0028eee54f, 0x3ff8561e39ed3156, 0x3ff8b0dc893b8941],
    ),
    (
        "la_qr_pivot",
        r#"lq$pivot"#,
        &[0x3ff0000000000000, 0x4008000000000000, 0x4000000000000000],
    ),
    (
        "qr_coef_real",
        r#".Internal(qr_coef_real(lq, ym))"#,
        &[
            0x3ff8d1ff7c0628de,
            0x40005e0a96300d0c,
            0x400904ba7a657f97,
            0xc000df13838b2610,
            0x4000936c6e5cf5dc,
            0x401b8bc9ac365330,
            0x4024a1c795c18d9b,
            0x40221d62d0eab802,
            0x4022d3d33f4c792e,
        ],
    ),
    (
        "qr_qy_real",
        r#".Internal(qr_qy_real(lq, ym, TRUE))"#,
        &[
            0xc009c8ce0fdec0d0,
            0xc0004adaa4c72158,
            0xc018cb5ebe16c9f5,
            0xc000df13838b2610,
            0x4000936c6e5cf5dc,
            0x401b8bc9ac365330,
            0x4024a1c795c18d9b,
            0x40221d62d0eab802,
            0x4022d3d33f4c792e,
        ],
    ),
    (
        "eigen_symmetric_values",
        r#".Internal(La_rs(s, FALSE))$values"#,
        &[0x3ff0d1c71972fe00, 0x4064731b0404b3ad, 0x40770886f412000f],
    ),
    (
        "eigen_symmetric_vectors",
        r#".Internal(La_rs(s, FALSE))$vectors"#,
        &[
            0x3fe3cf74a4c83809,
            0xbfe06970b8676804,
            0x3fe30812d643aaff,
            0x3fa70557cd6ca326,
            0x3fe8ef479c49ed1f,
            0x3fe4013950225e47,
            0x3fe916e427f08f35,
            0x3fd70eb65ae9bd44,
            0xbfe02ca1a8339855,
        ],
    ),
    (
        "eigen_general_real",
        r#"Re(.Internal(La_rg(a, TRUE))$values)"#,
        &[0x3fd33f5dd9226544, 0x40278b0258ea59d8, 0x40278b0258ea59d8],
    ),
    (
        "eigen_general_imaginary",
        r#"Im(.Internal(La_rg(a, TRUE))$values)"#,
        &[0x0000000000000000, 0x401af29453f629e5, 0xc01af29453f629e5],
    ),
    (
        "svd_d",
        r#".Internal(La_svd("S", x, rep(0, 3), u, v))$d"#,
        &[0x4042632916a16b7f, 0x40272fb466aba2a9, 0x3ff907b0057f44ac],
    ),
    (
        "svd_u",
        r#".Internal(La_svd("S", x, rep(0, 3), u, v))$u"#,
        &[
            0xbfdc6093ccdd785a,
            0xbfb37c8e3634c8f1,
            0x3fdd4c33a5562f02,
            0xbfaa25b9a622fea6,
            0xbfdc984d206c5fdb,
            0x3fc73e52fc5f4908,
            0x3fd91e5aa18be1ee,
            0xbfd16f386ca28e50,
            0xbfd69723d4d2641a,
            0xbfc2426ea0098ca5,
            0x3fe049c2a4bfbed2,
            0x3fb1e86d5708a6b4,
            0xbfdc2676de96b4f4,
            0x3fc1d4d1a5dd9220,
            0x3fd8b4506820ddd8,
            0xbfca7772ccefe39b,
            0xbfdbc0223855a995,
            0x3fd672e635eff9de,
            0xbfd23b1c70696287,
            0xbfe6104711983671,
            0xbfda5155a20a171a,
            0xbfdd52a44320e22c,
            0x3fc511252458a682,
            0x3fc202175a6567dc,
            0x3f96305d300fef14,
            0xbfc063db5c204ac6,
            0x3f9b185aa074792e,
        ],
    ),
    (
        "svd_vt",
        r#".Internal(La_svd("S", x, rep(0, 3), u, v))$vt"#,
        &[
            0xbfe2aca7e8439a9b,
            0xbfe6f8bf6e032c50,
            0xbfd84ba55ca31e4b,
            0x3fe382659d67ee2c,
            0xbfb4195c301e23ac,
            0xbfe93d8439ff18e1,
            0xbfe12a561f826a1c,
            0x3fe622beca1b2d88,
            0xbfdef147b26bd379,
        ],
    ),
    (
        "determinant",
        r#".Internal(det_ge_real(a, TRUE))$modulus"#,
        &[0x40100d806b942df0],
    ),
    (
        "dgecon",
        r#".Internal(La_dgecon(a, "O"))"#,
        &[0x3f80921032e63e6e],
    ),
    (
        "dtrcon",
        r#".Internal(La_dtrcon(a, "I"))"#,
        &[0x3fbcaf36170cac69],
    ),
    (
        "dlange",
        r#".Internal(La_dlange(x, "F"))"#,
        &[0x40434b95947c149a],
    ),
    (
        "complex_matprod_real",
        r#"Re(z %*% z)"#,
        &[
            0x0000000000000000,
            0x0000000000000000,
            0x0000000000000000,
            0x0000000000000000,
            0x0000000000000000,
            0x0000000000000000,
            0x0000000000000000,
            0x0000000000000000,
            0x0000000000000000,
        ],
    ),
    (
        "complex_matprod_imaginary",
        r#"Im(z %*% z)"#,
        &[
            0x4075ed25966418e9,
            0xc0715efff88c45b9,
            0xc071bba5c679aa85,
            0x40721bcaaaba6873,
            0xc0345718f8716af8,
            0xc0720d161b7106ec,
            0xc05c1e5376672000,
            0x40714b74f97286b4,
            0x404536fb4bf303b7,
        ],
    ),
    (
        "complex_solve_real",
        r#"Re(.Internal(La_solve_cmplx(z, b, 2.220446e-16)))"#,
        &[0x4010f2595635930f, 0xc00c1f0b84700619, 0x40109c447e0b9869],
    ),
    (
        "complex_solve_imaginary",
        r#"Im(.Internal(La_solve_cmplx(z, b, 2.220446e-16)))"#,
        &[0xc010f2595635930f, 0x400c1f0b84700619, 0xc0109c447e0b9869],
    ),
    (
        "complex_eigen_real",
        r#"Re(.Internal(La_rg_cmplx(z, TRUE))$values)"#,
        &[0x403282264172b772, 0x3fd33f5dd92265ba, 0x401423705dde89cc],
    ),
    (
        "complex_eigen_imaginary",
        r#"Im(.Internal(La_rg_cmplx(z, TRUE))$values)"#,
        &[0x401423705dde89de, 0x3fd33f5dd92264ff, 0x403282264172b76c],
    ),
    (
        "complex_svd_d",
        r#".Internal(La_svd_cmplx("N", z, rep(0, 3), w, w))$d"#,
        &[0x403b1cb27fe2ac3e, 0x4032087bc91b848a, 0x3fd47ba95e2329e9],
    ),
    (
        "complex_qr_real",
        r#"Re(.Internal(La_qr_cmplx(z))$qr)"#,
        &[
            0xc035460204164539,
            0xbfb4f90f8e015613,
            0xbfd84bd17a347ede,
            0xc02496aa62038cae,
            0xc02b57d43300c106,
            0xbfbf6f49bd737a22,
            0x402a860590a5d63d,
            0xc0278f12bf1c2881,
            0xbfe13768b7d0ec28,
        ],
    ),
];

/// Expressions GNU R rejects, with its messages.
const ERRORS: &[(&str, &str)] = &[
    (
        ".Internal(La_solve(singular, b[1:2], 2.220446e-16))",
        "Lapack routine dgesv: system is exactly singular: U[2,2] = 0",
    ),
    (
        ".Internal(La_solve(nearly_singular, b[1:2], 2.220446e-16))",
        "system is computationally singular: reciprocal condition number = 2.46716e-17",
    ),
    (
        ".Internal(La_chol(singular, FALSE, -1))",
        "the leading minor of order 2 is not positive",
    ),
    (
        r#".Fortran("dqrdc2", c(1, NA), 2L, 2L, 1L, 1e-7, 0L, 0, 1L, c(0, 0), PACKAGE = "base")"#,
        "NA/NaN/Inf in foreign function call (arg 1)",
    ),
    (
        r#".Fortran("dqrdc2", 1, PACKAGE = "base")"#,
        "Incorrect number of arguments (1), expecting 9 for 'dqrdc2'",
    ),
    (
        r#".Internal(La_dgecon(a, "X"))"#,
        "argument type[1]='X' must be one of '1','O', or 'I'",
    ),
    (
        ".Internal(backsolve(zero_diagonal, b[1:2], 2L, TRUE, FALSE))",
        "singular matrix in 'backsolve'. First zero in diagonal [2]",
    ),
];

#[test]
fn linear_algebra_matches_gnu_r() {
    if !system_lapack_present() {
        eprintln!(
            "skipping: the system BLAS and LAPACK (libblas.so.3, liblapack.so.3) are not installed"
        );
        return;
    }
    // SAFETY: the heap is leaked, so it outlives the interpreter, and this is the only test in
    // its process that uses the interpreter.
    unsafe {
        let heap: &'static mut Heap = Box::leak(Box::new(Heap::new()));
        interp::install(heap, interp::default_hooks());
        evaluate(SETUP).unwrap_or_else(|failure| panic!("the setup failed: {failure}"));
        for (name, expression, expected) in CASES {
            let value = evaluate(expression).unwrap_or_else(|failure| panic!("{name}: {failure}"));
            let bits: Vec<u64> = match value.kind() {
                SexpType::Real => values::reals(value)
                    .iter()
                    .map(|entry| entry.to_bits())
                    .collect(),
                SexpType::Integer => values::ints(value)
                    .iter()
                    .map(|&entry| f64::from(entry).to_bits())
                    .collect(),
                other => panic!("{name}: `{expression}` returned a {}", other.name()),
            };
            assert_eq!(&bits, expected, "{name}: `{expression}`");
        }
        // Without base R's handlers an error goes to the default handler, which records the
        // message for `geterrmessage()` and jumps to top level.
        interp::interp().show_error_messages = false;
        for (expression, message) in ERRORS {
            if evaluate(expression).is_ok() {
                panic!("`{expression}` succeeded, but GNU R reports: {message}");
            }
            let reported = &interp::interp().error_message;
            assert!(
                reported.contains(message),
                "`{expression}` reported {reported:?}, not {message:?}"
            );
        }
    }
}

/// Evaluates `text` in the global environment and returns the last value, or the error message.
unsafe fn evaluate(text: &str) -> Result<Sexp, String> {
    let message = |unwind: Unwind| match unwind {
        Unwind::Error(failure) => failure.message,
        _ => "a jump to top level".to_owned(),
    };
    // SAFETY: the interpreter is installed, and every handle comes from its heap.
    unsafe {
        let expressions = lower::parse_text(text, "<test>").map_err(message)?;
        let mut value = values::nil();
        for index in 0..values::length(expressions) {
            value =
                eval::eval(expressions.element(index), values::global_env()).map_err(message)?;
        }
        Ok(value)
    }
}

fn system_lapack_present() -> bool {
    [c"libblas.so.3", c"liblapack.so.3"].iter().all(|name| {
        // SAFETY: probing for a library has no preconditions; the handle stays open for the test.
        unsafe { !libc::dlopen(name.as_ptr(), libc::RTLD_NOW).is_null() }
    })
}
