//! The fast Fourier transform behind `fft` and `mvfft`, which must give GNU R's bits: the
//! butterflies, the twiddle recurrences and the permutation order decide every rounding error.
//!
//! This file is a modified translation into Rust of GNU R's `src/library/stats/src/fft.c`, which
//! is Richard Singleton's mixed-radix algorithm ("Programs for Digital Signal Processing", IEEE
//! Press) as translated to C by Ross Ihaka. The Fortran-style `goto` structure is kept as a
//! state machine over the original labels, so the arithmetic can be compared line by line.
//!
//!     Copyright (C) 1998--2025 The R Core Team
//!     Copyright (C) 1995, 1996, 1997 Robert Gentleman and Ross Ihaka

use std::f64::consts::FRAC_PI_4;

/// Why a transform did not run.
#[derive(Debug, PartialEq, Eq)]
pub enum FftError {
    /// The length has more prime factors than the algorithm has room for.
    Factorization,
}

/// A transform of the interleaved (real, imaginary) pairs in `data`.
pub fn fft_vector(data: &mut [f64], inverse: bool) -> Result<(), FftError> {
    let count = data.len() / 2;
    if count > 1 {
        transform(data, 1, count, 1, inverse)?;
    }
    Ok(())
}

/// The transform of every dimension of an array whose extents are `dims`, one dimension after
/// the other.
pub fn fft_array(data: &mut [f64], dims: &[usize], inverse: bool) -> Result<(), FftError> {
    for &extent in dims {
        if extent > 1 {
            factorize(extent)?;
        }
    }
    let mut segments = data.len() / 2;
    let mut extent = 1;
    let mut spacing = 1;
    for &dim in dims {
        if dim > 1 {
            spacing *= extent;
            extent = dim;
            segments /= extent;
            transform(data, segments, extent, spacing, inverse)?;
        }
    }
    Ok(())
}

/// The transform of each of the `columns` columns of `rows` values.
pub fn fft_columns(
    data: &mut [f64],
    rows: usize,
    columns: usize,
    inverse: bool,
) -> Result<(), FftError> {
    if rows > 1 {
        factorize(rows)?;
        for column in 0..columns {
            transform(&mut data[2 * column * rows..], 1, rows, 1, inverse)?;
        }
    }
    Ok(())
}

fn transform(
    data: &mut [f64],
    segments: usize,
    extent: usize,
    spacing: usize,
    inverse: bool,
) -> Result<(), FftError> {
    let factors = factorize(extent)?;
    let span = extent * spacing;
    let total = span * segments;
    let sign = if inverse { 2 } else { -2 };
    butterflies(
        data,
        total as i64,
        extent as i64,
        span as i64,
        sign,
        factors,
    );
    Ok(())
}

/// The factors of the series length: the square factors first, then the square-free ones, then
/// the square factors again, with the counts the passes need.
struct Factors {
    nfac: [i64; 24],
    count: usize,
    squares: usize,
    max_factor: usize,
    max_permutation: usize,
}

/// GNU R's `fft_factor`.
fn factorize(n: usize) -> Result<Factors, FftError> {
    let mut nfac = [0i64; 24];
    let mut count = 0usize;
    let mut remaining = n as i64;
    let mut max_permutation = 0i64;
    let squares;
    while remaining % 16 == 0 {
        nfac[count] = 4;
        count += 1;
        remaining /= 16;
    }
    let mut sqrt_remaining = (remaining as f64).sqrt() as i64;
    let mut divisor = 3;
    while divisor <= sqrt_remaining {
        let square = divisor * divisor;
        let mut changed = false;
        while remaining % square == 0 {
            nfac[count] = divisor;
            count += 1;
            remaining /= square;
            changed = true;
        }
        if changed {
            sqrt_remaining = (remaining as f64).sqrt() as i64;
        }
        divisor += 2;
    }
    if remaining <= 4 {
        squares = count;
        nfac[count] = remaining;
        if remaining != 1 {
            count += 1;
        }
    } else {
        if remaining % 4 == 0 {
            nfac[count] = 2;
            count += 1;
            remaining /= 4;
        }
        squares = count;
        max_permutation = (2 * squares as i64 + 2).max(remaining - 1);
        let mut j = 2;
        loop {
            if remaining % j == 0 {
                nfac[count] = j;
                count += 1;
                remaining /= j;
            }
            if j > i32::MAX as i64 - 2 {
                break;
            }
            j = ((j + 1) / 2) * 2 + 1;
            if j > remaining {
                break;
            }
        }
    }
    if count <= squares + 1 {
        max_permutation = (count + squares + 1) as i64;
    }
    if count + squares > 20 {
        return Err(FftError::Factorization);
    }
    let mut j = squares;
    while j != 0 {
        j -= 1;
        nfac[count] = nfac[j];
        count += 1;
    }
    let mut max_factor = nfac[count - squares - 1];
    if squares > 0 {
        max_factor = max_factor.max(nfac[squares - 1]);
    }
    if squares > 1 {
        max_factor = max_factor.max(nfac[squares - 2]);
    }
    if squares > 2 {
        max_factor = max_factor.max(nfac[squares - 3]);
    }
    Ok(Factors {
        nfac,
        count,
        squares,
        max_factor: max_factor as usize,
        max_permutation: max_permutation as usize,
    })
}

#[derive(Clone, Copy)]
enum Step {
    Start,
    L60,
    L70,
    L80,
    L100,
    L110,
    L120,
    L130,
    L140,
    L150,
    L160,
    L170,
    L180,
    L190,
    L200,
    Five,
    L220,
    Odd,
    L250,
    L290,
    L300,
    Finish,
    Permute370,
    Permute380,
    Permute400,
    Permute410,
    L440,
    L460,
    L470,
    L480,
    Cycles,
    Reorder,
    L520,
    L570,
}

/// GNU R's `fftmx`: `data` holds `ntot` interleaved complex values, `n` is the length of the
/// dimension transformed, `nspan` the distance from one such value to the next in units of
/// values, and `isn` is -2 or 2 for the sign of the exponent. Variable names follow the
/// original so the two can be read side by side.
#[allow(
    clippy::cognitive_complexity,
    clippy::too_many_lines,
    unused_assignments
)]
fn butterflies(data: &mut [f64], ntot: i64, n: i64, nspan: i64, isn: i64, factors: Factors) {
    let Factors {
        mut nfac,
        count: m,
        squares: kt,
        max_factor: scratch,
        max_permutation,
    } = factors;
    let m = m as i64;
    let mut kt = kt as i64;
    let scratch = scratch as i64;
    let mut work = vec![0.0f64; 4 * scratch as usize + 8];
    let mut np = vec![0i64; max_permutation + 2 * 24 + 8];

    macro_rules! a {
        ($k:expr) => {
            data[(($k) - 1) as usize]
        };
    }
    macro_rules! b {
        ($k:expr) => {
            data[($k) as usize]
        };
    }
    macro_rules! at {
        ($k:expr) => {
            work[(($k) - 1) as usize]
        };
    }
    macro_rules! ck {
        ($k:expr) => {
            work[(scratch + ($k) - 1) as usize]
        };
    }
    macro_rules! bt {
        ($k:expr) => {
            work[(2 * scratch + ($k) - 1) as usize]
        };
    }
    macro_rules! sk {
        ($k:expr) => {
            work[(3 * scratch + ($k) - 1) as usize]
        };
    }
    macro_rules! np {
        ($k:expr) => {
            np[($k) as usize]
        };
    }

    let (mut aa, mut aj, mut ajm, mut ajp, mut ak, mut akm, mut akp);
    let (mut bb, mut bj, mut bjm, mut bjp, mut bk, mut bkm, mut bkp);
    let (mut c1, mut c2, mut c3) = (0.0f64, 0.0f64, 0.0f64);
    let (mut s1, mut s2, mut s3) = (0.0f64, 0.0f64, 0.0f64);
    let (mut j, mut jj, mut k, mut k1, mut k2, mut k3, mut k4, mut kk);
    aa = 0.0;
    aj = 0.0;
    ajm = 0.0;
    ajp = 0.0;
    ak = 0.0;
    akm = 0.0;
    akp = 0.0;
    bb = 0.0;
    bj = 0.0;
    bjm = 0.0;
    bjp = 0.0;
    bk = 0.0;
    bkm = 0.0;
    bkp = 0.0;
    j = 0i64;
    jj = 0i64;
    k = 0i64;
    k1 = 0i64;
    k2 = 0i64;
    k3 = 0i64;
    k4 = 0i64;
    kk = 0i64;

    let inc = isn.abs();
    let mut nt = inc * ntot;
    let ks = inc * nspan;
    let mut rad = FRAC_PI_4;
    let mut s72 = rad / 0.625;
    let c72 = s72.cos();
    s72 = s72.sin();
    let mut s120 = 0.5 * 1.732_050_807_568_877_293_527_446_341_506_f64;
    if isn <= 0 {
        s72 = -s72;
        s120 = -s120;
        rad = -rad;
    }

    let mut kspan = ks;
    let nn = nt - inc;
    let mut nn_cycle = nn;
    let mut jc = ks / n;
    let lim = 32;
    let klim = lim * jc;
    let mut i = 0i64;
    let mut jf = 0i64;
    let mut maxf = nfac[(m - kt - 1) as usize];
    if kt > 0 {
        maxf = maxf.max(nfac[(kt - 1) as usize]);
    }
    let mut dr = 0.0f64;
    let mut cd = 0.0f64;
    let mut sd = 0.0f64;
    let mut mm = 0i64;
    let mut kspnn = 0i64;
    let mut step = Step::Start;

    'machine: loop {
        match step {
            Step::Start => {
                dr = (8.0 * jc as f64) / kspan as f64;
                cd = (0.5 * dr * rad).sin();
                cd = 2.0 * cd * cd;
                sd = (dr * rad).sin();
                kk = 1;
                i += 1;
                if nfac[(i - 1) as usize] != 2 {
                    step = Step::L110;
                    continue;
                }
                // Factor of 2, with its rotation factor.
                kspan /= 2;
                k1 = kspan + 2;
                loop {
                    loop {
                        k2 = kk + kspan;
                        ak = a!(k2);
                        bk = b!(k2);
                        a!(k2) = a!(kk) - ak;
                        b!(k2) = b!(kk) - bk;
                        a!(kk) += ak;
                        b!(kk) += bk;
                        kk = k2 + kspan;
                        if kk > nn {
                            break;
                        }
                    }
                    kk -= nn;
                    if kk > jc {
                        break;
                    }
                }
                if kk > kspan {
                    step = Step::Finish;
                    continue;
                }
                step = Step::L60;
            }
            Step::L60 => {
                c1 = 1.0 - cd;
                s1 = sd;
                mm = (k1 / 2).min(klim);
                step = Step::L80;
            }
            Step::L70 => {
                ak = c1 - (cd * c1 + sd * s1);
                s1 = (sd * c1 - cd * s1) + s1;
                c1 = ak;
                step = Step::L80;
            }
            Step::L80 => {
                loop {
                    k2 = kk + kspan;
                    ak = a!(kk) - a!(k2);
                    bk = b!(kk) - b!(k2);
                    a!(kk) += a!(k2);
                    b!(kk) += b!(k2);
                    a!(k2) = c1 * ak - s1 * bk;
                    b!(k2) = s1 * ak + c1 * bk;
                    kk = k2 + kspan;
                    if kk >= nt {
                        break;
                    }
                }
                k2 = kk - nt;
                c1 = -c1;
                kk = k1 - k2;
                if kk > k2 {
                    continue 'machine;
                }
                kk += jc;
                if kk <= mm {
                    step = Step::L70;
                    continue;
                }
                if kk >= k2 {
                    k1 = k1 + inc + inc;
                    kk = (k1 - kspan) / 2 + jc;
                    if kk <= jc + jc {
                        step = Step::L60;
                    } else {
                        step = Step::Start;
                    }
                    continue;
                }
                s1 = ((kk - 1) / jc) as f64 * dr * rad;
                c1 = s1.cos();
                s1 = s1.sin();
                mm = (k1 / 2).min(mm + klim);
            }
            Step::L100 => {
                // Factor of 3.
                k1 = kk + kspan;
                k2 = k1 + kspan;
                ak = a!(kk);
                bk = b!(kk);
                aj = a!(k1) + a!(k2);
                bj = b!(k1) + b!(k2);
                a!(kk) = ak + aj;
                b!(kk) = bk + bj;
                ak = -0.5 * aj + ak;
                bk = -0.5 * bj + bk;
                aj = (a!(k1) - a!(k2)) * s120;
                bj = (b!(k1) - b!(k2)) * s120;
                a!(k1) = ak - bj;
                b!(k1) = bk + aj;
                a!(k2) = ak + bj;
                b!(k2) = bk - aj;
                kk = k2 + kspan;
                if kk < nn {
                    continue;
                }
                kk -= nn;
                if kk <= kspan {
                    continue;
                }
                step = Step::L290;
            }
            Step::L110 => {
                if nfac[(i - 1) as usize] != 4 {
                    step = Step::Odd;
                    continue;
                }
                // Factor of 4.
                kspnn = kspan;
                kspan /= 4;
                step = Step::L120;
            }
            Step::L120 => {
                c1 = 1.0;
                s1 = 0.0;
                mm = kspan.min(klim);
                step = Step::L150;
            }
            Step::L130 => {
                c2 = c1 - (cd * c1 + sd * s1);
                s1 = (sd * c1 - cd * s1) + s1;
                c1 = c2;
                step = Step::L140;
            }
            Step::L140 => {
                c2 = c1 * c1 - s1 * s1;
                s2 = c1 * s1 * 2.0;
                c3 = c2 * c1 - s2 * s1;
                s3 = c2 * s1 + s2 * c1;
                step = Step::L150;
            }
            Step::L150 => {
                k1 = kk + kspan;
                k2 = k1 + kspan;
                k3 = k2 + kspan;
                akp = a!(kk) + a!(k2);
                akm = a!(kk) - a!(k2);
                ajp = a!(k1) + a!(k3);
                ajm = a!(k1) - a!(k3);
                a!(kk) = akp + ajp;
                ajp = akp - ajp;
                bkp = b!(kk) + b!(k2);
                bkm = b!(kk) - b!(k2);
                bjp = b!(k1) + b!(k3);
                bjm = b!(k1) - b!(k3);
                b!(kk) = bkp + bjp;
                bjp = bkp - bjp;
                if isn < 0 {
                    step = Step::L180;
                    continue;
                }
                akp = akm - bjm;
                akm += bjm;
                bkp = bkm + ajm;
                bkm -= ajm;
                if s1 == 0.0 {
                    step = Step::L190;
                } else {
                    step = Step::L160;
                }
            }
            Step::L160 => {
                a!(k1) = akp * c1 - bkp * s1;
                b!(k1) = akp * s1 + bkp * c1;
                a!(k2) = ajp * c2 - bjp * s2;
                b!(k2) = ajp * s2 + bjp * c2;
                a!(k3) = akm * c3 - bkm * s3;
                b!(k3) = akm * s3 + bkm * c3;
                kk = k3 + kspan;
                if kk <= nt {
                    step = Step::L150;
                } else {
                    step = Step::L170;
                }
            }
            Step::L170 => {
                kk = kk - nt + jc;
                if kk <= mm {
                    step = Step::L130;
                    continue;
                }
                if kk < kspan {
                    step = Step::L200;
                    continue;
                }
                kk = kk - kspan + inc;
                if kk <= jc {
                    step = Step::L120;
                    continue;
                }
                if kspan == jc {
                    step = Step::Finish;
                } else {
                    step = Step::Start;
                }
            }
            Step::L180 => {
                akp = akm + bjm;
                akm -= bjm;
                bkp = bkm - ajm;
                bkm += ajm;
                if s1 != 0.0 {
                    step = Step::L160;
                } else {
                    step = Step::L190;
                }
            }
            Step::L190 => {
                a!(k1) = akp;
                b!(k1) = bkp;
                a!(k2) = ajp;
                b!(k2) = bjp;
                a!(k3) = akm;
                b!(k3) = bkm;
                kk = k3 + kspan;
                if kk <= nt {
                    step = Step::L150;
                } else {
                    step = Step::L170;
                }
            }
            Step::L200 => {
                s1 = ((kk - 1) / jc) as f64 * dr * rad;
                c1 = s1.cos();
                s1 = s1.sin();
                mm = kspan.min(mm + klim);
                step = Step::L140;
            }
            Step::Five => {
                c2 = c72 * c72 - s72 * s72;
                s2 = 2.0 * c72 * s72;
                step = Step::L220;
            }
            Step::L220 => {
                k1 = kk + kspan;
                k2 = k1 + kspan;
                k3 = k2 + kspan;
                k4 = k3 + kspan;
                akp = a!(k1) + a!(k4);
                akm = a!(k1) - a!(k4);
                bkp = b!(k1) + b!(k4);
                bkm = b!(k1) - b!(k4);
                ajp = a!(k2) + a!(k3);
                ajm = a!(k2) - a!(k3);
                bjp = b!(k2) + b!(k3);
                bjm = b!(k2) - b!(k3);
                aa = a!(kk);
                bb = b!(kk);
                a!(kk) = aa + akp + ajp;
                b!(kk) = bb + bkp + bjp;
                ak = akp * c72 + ajp * c2 + aa;
                bk = bkp * c72 + bjp * c2 + bb;
                aj = akm * s72 + ajm * s2;
                bj = bkm * s72 + bjm * s2;
                a!(k1) = ak - bj;
                a!(k4) = ak + bj;
                b!(k1) = bk + aj;
                b!(k4) = bk - aj;
                ak = akp * c2 + ajp * c72 + aa;
                bk = bkp * c2 + bjp * c72 + bb;
                aj = akm * s2 - ajm * s72;
                bj = bkm * s2 - bjm * s72;
                a!(k2) = ak - bj;
                a!(k3) = ak + bj;
                b!(k2) = bk + aj;
                b!(k3) = bk - aj;
                kk = k4 + kspan;
                if kk < nn {
                    continue;
                }
                kk -= nn;
                if kk <= kspan {
                    continue;
                }
                step = Step::L290;
            }
            Step::Odd => {
                k = nfac[(i - 1) as usize];
                kspnn = kspan;
                kspan /= k;
                if k == 3 {
                    step = Step::L100;
                    continue;
                }
                if k == 5 {
                    step = Step::Five;
                    continue;
                }
                if k == jf {
                    step = Step::L250;
                    continue;
                }
                jf = k;
                s1 = rad / (k as f64 / 8.0);
                c1 = s1.cos();
                s1 = s1.sin();
                ck!(jf) = 1.0;
                sk!(jf) = 0.0;
                j = 1;
                while j < k {
                    ck!(j) = ck!(k) * c1 + sk!(k) * s1;
                    sk!(j) = ck!(k) * s1 - sk!(k) * c1;
                    k -= 1;
                    ck!(k) = ck!(j);
                    sk!(k) = -sk!(j);
                    j += 1;
                }
                step = Step::L250;
            }
            Step::L250 => {
                k1 = kk;
                k2 = kk + kspnn;
                aa = a!(kk);
                bb = b!(kk);
                ak = aa;
                bk = bb;
                j = 1;
                k1 += kspan;
                loop {
                    k2 -= kspan;
                    j += 1;
                    at!(j) = a!(k1) + a!(k2);
                    ak = at!(j) + ak;
                    bt!(j) = b!(k1) + b!(k2);
                    bk = bt!(j) + bk;
                    j += 1;
                    at!(j) = a!(k1) - a!(k2);
                    bt!(j) = b!(k1) - b!(k2);
                    k1 += kspan;
                    if k1 >= k2 {
                        break;
                    }
                }
                a!(kk) = ak;
                b!(kk) = bk;
                k1 = kk;
                k2 = kk + kspnn;
                j = 1;
                loop {
                    k1 += kspan;
                    k2 -= kspan;
                    jj = j;
                    ak = aa;
                    bk = bb;
                    aj = 0.0;
                    bj = 0.0;
                    k = 2;
                    while k < jf {
                        ak += at!(k) * ck!(jj);
                        bk += bt!(k) * ck!(jj);
                        k += 1;
                        aj += at!(k) * sk!(jj);
                        bj += bt!(k) * sk!(jj);
                        jj += j;
                        if jj > jf {
                            jj -= jf;
                        }
                        k += 1;
                    }
                    k = jf - j;
                    a!(k1) = ak - bj;
                    b!(k1) = bk + aj;
                    a!(k2) = ak + bj;
                    b!(k2) = bk - aj;
                    j += 1;
                    if j >= k {
                        break;
                    }
                }
                kk += kspnn;
                if kk <= nn {
                    continue;
                }
                kk -= nn;
                if kk <= kspan {
                    continue;
                }
                step = Step::L290;
            }
            Step::L290 => {
                // Multiply by the rotation factor, except for factors of 2 and 4.
                if i == m {
                    step = Step::Finish;
                    continue;
                }
                kk = jc + 1;
                step = Step::L300;
            }
            Step::L300 => {
                c2 = 1.0 - cd;
                s1 = sd;
                mm = kspan.min(klim);
                loop {
                    c1 = c2;
                    s2 = s1;
                    kk += kspan;
                    loop {
                        loop {
                            ak = a!(kk);
                            a!(kk) = c2 * ak - s2 * b!(kk);
                            b!(kk) = s2 * ak + c2 * b!(kk);
                            kk += kspnn;
                            if kk > nt {
                                break;
                            }
                        }
                        ak = s1 * s2;
                        s2 = s1 * c2 + c1 * s2;
                        c2 = c1 * c2 - ak;
                        kk += -nt + kspan;
                        if kk > kspnn {
                            break;
                        }
                    }
                    kk += -kspnn + jc;
                    if kk <= mm {
                        c2 = c1 - (cd * c1 + sd * s1);
                        s1 += sd * c1 - cd * s1;
                        continue;
                    }
                    if kk >= kspan {
                        kk = kk - kspan + jc + inc;
                        if kk <= jc + jc {
                            step = Step::L300;
                        } else {
                            step = Step::Start;
                        }
                        continue 'machine;
                    }
                    s1 = ((kk - 1) / jc) as f64 * dr * rad;
                    c2 = s1.cos();
                    s1 = s1.sin();
                    mm = kspan.min(mm + klim);
                }
            }
            Step::Finish => {
                // Permute the results to normal order, first for the square factors.
                np!(1) = ks;
                if kt == 0 {
                    step = Step::L440;
                    continue;
                }
                k = kt + kt + 1;
                if m < k {
                    k -= 1;
                }
                np!(k + 1) = jc;
                j = 1;
                while j < k {
                    np!(j + 1) = np!(j) / nfac[(j - 1) as usize];
                    np!(k) = np!(k + 1) * nfac[(j - 1) as usize];
                    j += 1;
                    k -= 1;
                }
                k3 = np!(k + 1);
                kspan = np!(2);
                kk = jc + 1;
                k2 = kspan + 1;
                j = 1;
                step = if n == ntot {
                    Step::Permute370
                } else {
                    Step::Permute400
                };
            }
            Step::Permute370 => {
                loop {
                    ak = a!(kk);
                    a!(kk) = a!(k2);
                    a!(k2) = ak;
                    bk = b!(kk);
                    b!(kk) = b!(k2);
                    b!(k2) = bk;
                    kk += inc;
                    k2 += kspan;
                    if k2 >= ks {
                        break;
                    }
                }
                step = Step::Permute380;
            }
            Step::Permute380 => {
                loop {
                    k2 -= np!(j);
                    j += 1;
                    k2 += np!(j + 1);
                    if k2 <= np!(j) {
                        break;
                    }
                }
                j = 1;
                loop {
                    if kk < k2 {
                        step = Step::Permute370;
                        continue 'machine;
                    }
                    kk += inc;
                    k2 += kspan;
                    if k2 >= ks {
                        break;
                    }
                }
                if kk < ks {
                    continue;
                }
                jc = k3;
                step = Step::L440;
            }
            Step::Permute400 => {
                k = kk + jc;
                loop {
                    ak = a!(kk);
                    a!(kk) = a!(k2);
                    a!(k2) = ak;
                    bk = b!(kk);
                    b!(kk) = b!(k2);
                    b!(k2) = bk;
                    kk += inc;
                    k2 += inc;
                    if kk >= k {
                        break;
                    }
                }
                kk += ks - jc;
                k2 += ks - jc;
                if kk < nt {
                    continue;
                }
                k2 += -nt + kspan;
                kk += -nt + jc;
                if k2 < ks {
                    continue;
                }
                step = Step::Permute410;
            }
            Step::Permute410 => {
                loop {
                    loop {
                        k2 -= np!(j);
                        j += 1;
                        k2 += np!(j + 1);
                        if k2 <= np!(j) {
                            break;
                        }
                    }
                    j = 1;
                    loop {
                        if kk < k2 {
                            step = Step::Permute400;
                            continue 'machine;
                        }
                        kk += jc;
                        k2 += kspan;
                        if k2 >= ks {
                            break;
                        }
                    }
                    if kk >= ks {
                        break;
                    }
                }
                jc = k3;
                step = Step::L440;
            }
            Step::L440 => {
                if 2 * kt + 1 >= m {
                    return;
                }
                kspnn = np!(kt + 1);
                // Permutation for the square-free factors. From here `nfac` holds the
                // cumulative products of the factors.
                nn_cycle = m - kt;
                nfac[nn_cycle as usize] = 1;
                j = nn_cycle;
                while j > kt {
                    nfac[(j - 1) as usize] *= nfac[j as usize];
                    j -= 1;
                }
                kt += 1;
                nn_cycle = nfac[(kt - 1) as usize] - 1;
                jj = 0;
                j = 0;
                step = Step::L480;
            }
            Step::L460 => {
                jj -= k2;
                k2 = kk;
                k += 1;
                kk = nfac[(k - 1) as usize];
                step = Step::L470;
            }
            Step::L470 => {
                jj += kk;
                if jj >= k2 {
                    step = Step::L460;
                    continue;
                }
                np!(j) = jj;
                step = Step::L480;
            }
            Step::L480 => {
                k2 = nfac[(kt - 1) as usize];
                k = kt + 1;
                kk = nfac[(k - 1) as usize];
                j += 1;
                if j <= nn_cycle {
                    step = Step::L470;
                    continue;
                }
                // The permutation cycles of length greater than 1.
                j = 0;
                step = Step::Cycles;
            }
            Step::Cycles => {
                loop {
                    j += 1;
                    kk = np!(j);
                    if kk >= 0 {
                        break;
                    }
                }
                if kk != j {
                    loop {
                        k = kk;
                        kk = np!(k);
                        np!(k) = -kk;
                        if kk == j {
                            break;
                        }
                    }
                    k3 = kk;
                    continue;
                }
                np!(j) = -j;
                if j != nn_cycle {
                    continue;
                }
                maxf *= inc;
                step = Step::L570;
            }
            Step::Reorder => {
                loop {
                    j -= 1;
                    if np!(j) >= 0 {
                        break;
                    }
                }
                jj = jc;
                step = Step::L520;
            }
            Step::L520 => {
                kspan = jj.min(maxf);
                jj -= kspan;
                k = np!(j);
                kk = jc * k + i + jj;
                k1 = kk + kspan;
                k2 = 1;
                while k1 != kk {
                    at!(k2) = a!(k1);
                    bt!(k2) = b!(k1);
                    k1 -= inc;
                    k2 += 1;
                }
                loop {
                    k1 = kk + kspan;
                    k2 = k1 - jc * (k + np!(k));
                    k = -np!(k);
                    loop {
                        a!(k1) = a!(k2);
                        b!(k1) = b!(k2);
                        k1 -= inc;
                        k2 -= inc;
                        if k1 == kk {
                            break;
                        }
                    }
                    kk = k2;
                    if k == j {
                        break;
                    }
                }
                k1 = kk + kspan;
                k2 = 1;
                while k1 > kk {
                    a!(k1) = at!(k2);
                    b!(k1) = bt!(k2);
                    k1 -= inc;
                    k2 += 1;
                }
                if jj != 0 {
                    continue;
                }
                if j != 1 {
                    step = Step::Reorder;
                    continue;
                }
                step = Step::L570;
            }
            Step::L570 => {
                j = k3 + 1;
                nt -= kspnn;
                i = nt - inc + 1;
                if nt >= 0 {
                    step = Step::Reorder;
                    continue;
                }
                return;
            }
        }
    }
}
