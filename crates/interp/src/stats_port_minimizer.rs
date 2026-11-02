//! The PORT library's general minimizers, written from the netlib reference
//! (<https://netlib.org/port/>) so that `nlminb` takes GNU R's iterates exactly.
//!
//! `nlminb` picks one of six PORT drivers. With a gradient it runs `DRMNG` (unconstrained) or
//! `DRMNGB` (bounds), which keep a BFGS approximation of the Hessian as its Cholesky factor
//! and take double-dogleg steps. With a gradient and a Hessian it runs `DRMNH` or `DRMNHB`,
//! which take More-Hebden trust-region steps on the exact Hessian. Without a gradient it runs
//! `DRMNF` or `DRMNFB`, which wrap the secant drivers and estimate each gradient by finite
//! differences with Stewart's step sizes. [`Minimizer`] covers all six behind PORT's
//! reverse-communication protocol: [`Minimizer::advance`] returns 1 to ask for the objective
//! at [`Minimizer::x`], 2 to ask for the gradient (or, while differencing, for another
//! objective value), and 3 or more when the run is over, with PORT's return code.
//!
//! The floating-point operations follow the reference statement by statement, in the same
//! order and association, because `nlminb`'s results are compared with GNU R's to the last
//! bit. The routines keep PORT's names, and so do the fields that stand for PORT's `IV` and
//! `V` slots and the local variables of the transliterated routines, so that each line can be
//! checked against the reference. Labelled control flow keeps PORT's statement numbers for
//! the same reason. Index arithmetic inside a routine stays 1-based where the reference's
//! packed-triangle offsets are easier to check that way.

use crate::{format::c_general, write_console};

/// What a minimizer evaluates besides the objective.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Derivatives {
    /// Neither gradient nor Hessian: finite-difference gradients (`DRMNF`, `DRMNFB`).
    None,
    /// The gradient (`DRMNG`, `DRMNGB`).
    Gradient,
    /// The gradient and the Hessian (`DRMNH`, `DRMNHB`).
    Hessian,
}

/// The settings that `port_ivset` and `nlminb`'s `control` leave in `iv` and `v`.
pub struct Settings {
    pub mxfcal: i32,
    pub mxiter: i32,
    pub outlev: i32,
    pub inith: i32,
    /// `V(19)` through `V(43)`.
    pub tolerances: [f64; 25],
}

/// One run of a PORT minimizer, driven through reverse communication.
pub struct Minimizer {
    n: usize,
    derivatives: Derivatives,
    lower: Vec<f64>,
    upper: Vec<f64>,
    bounded: bool,
    trace: i32,
    /// The scale vector `D`.
    d: Vec<f64>,
    /// The current point; the caller evaluates there.
    pub x: Vec<f64>,
    /// The objective at `x`, which the caller stores before the next `advance`.
    pub fx: f64,
    /// The gradient at `x`, which the caller stores when asked for it.
    pub g: Vec<f64>,
    /// The Hessian's lower triangle packed by rows, stored with the gradient.
    pub h: Vec<f64>,
    /// `IV(1)`: the last return code, which is also the next call's input.
    code: i32,
    iv: Counters,
    v: Scalars,
    /// The Cholesky factor `L`, its lower triangle packed by rows.
    l: Vec<f64>,
    /// `X0`, with a second half of scratch the bounded step routines use.
    x0: Vec<f64>,
    /// `STEP`; the bounded routines use the second third, and the last third is `STLSTG`.
    step: Vec<f64>,
    /// `NWTSTP`. Its second half is `G0`, which `DRMNGB` also hands to `DD7DGB` and
    /// `DQ7RSH` as scratch, so a step that adds a bound overwrites the old gradient before
    /// the BFGS update reads it. Keeping the two in one buffer reproduces that.
    nwtstp: Vec<f64>,
    /// `DG`, with a second half the bounded routines use.
    dg: Vec<f64>,
    /// The `W` work vector of `DG7QTS` and `DG7QSB`, which keeps values between calls.
    work: Vec<f64>,
    perm: Vec<i32>,
    ipiv1: Vec<i32>,
    ipiv2: Vec<i32>,
    differences: Differences,
}

impl Minimizer {
    pub fn new(
        settings: &Settings,
        derivatives: Derivatives,
        start: &[f64],
        d: &[f64],
        bounds: Option<(&[f64], &[f64])>,
    ) -> Minimizer {
        let n = start.len();
        let (lower, upper) = match bounds {
            Some((lower, upper)) => (lower.to_vec(), upper.to_vec()),
            None => (Vec::new(), Vec::new()),
        };
        let t = &settings.tolerances;
        Minimizer {
            n,
            derivatives,
            bounded: bounds.is_some(),
            lower,
            upper,
            trace: settings.outlev,
            d: d.to_vec(),
            x: start.to_vec(),
            fx: 0.0,
            g: vec![0.0; n],
            h: vec![0.0; n * (n + 1) / 2],
            code: 12,
            iv: Counters {
                mxfcal: settings.mxfcal,
                mxiter: settings.mxiter,
                inith: settings.inith,
                ..Counters::default()
            },
            v: Scalars {
                epslon: t[0],
                phmnfc: t[1],
                phmxfc: t[2],
                decfac: t[3],
                incfac: t[4],
                rdfcmn: t[5],
                rdfcmx: t[6],
                tuner1: t[7],
                tuner2: t[8],
                tuner3: t[9],
                tuner4: t[10],
                tuner5: t[11],
                afctol: t[12],
                rfctol: t[13],
                xctol: t[14],
                xftol: t[15],
                lmax0: t[16],
                lmaxs: t[17],
                sctol: t[18],
                dinit: t[19],
                eta0: t[23],
                bias: t[24],
                ..Scalars::default()
            },
            l: vec![0.0; n * (n + 1) / 2],
            x0: vec![0.0; 2 * n],
            step: vec![0.0; 3 * n],
            nwtstp: vec![0.0; 2 * n],
            dg: vec![0.0; 2 * n],
            work: vec![0.0; 4 * n + 7],
            perm: vec![0; n],
            ipiv1: vec![0; n],
            ipiv2: vec![0; n],
            differences: Differences {
                alpha: vec![0.0; n],
                w: [0.0; 6],
                sgirc: 0,
            },
        }
        .checked(settings)
    }

    /// `DPARCK`'s checks: the code is the last `V` slot out of range, or 18 for a
    /// nonpositive scale, and the run ends before any evaluation.
    fn checked(mut self, settings: &Settings) -> Minimizer {
        let tiny = f64::MIN_POSITIVE;
        let machep = f64::EPSILON;
        let big = f64::MAX;
        let ranges: [(f64, f64); 25] = [
            (1.0e-3, 0.9),
            (-0.99, -1.0e-3),
            (1.0e-3, 1.0e1),
            (1.0e-2, 0.8),
            (1.2, 1.0e2),
            (1.0e-2, 0.8),
            (1.2, 1.0e2),
            (0.0, 0.5),
            (0.0, 0.5),
            (1.0e-3, 1.0),
            (-1.0, 1.0),
            (machep, big),
            (0.0, big),
            (machep, 0.1),
            (0.0, 1.0),
            (0.0, 1.0),
            (tiny, big),
            (tiny, big),
            (0.0, 1.0),
            (-10.0, big),
            (0.0, big),
            (0.0, big),
            (0.0, 1.0),
            (machep, 1.0),
            (0.0, 1.0),
        ];
        let mut m = 0;
        for (k, (value, (low, high))) in settings.tolerances.iter().zip(ranges).enumerate() {
            if !(*value >= low && *value <= high) {
                m = k as i32 + 19;
            }
        }
        if !(self.v.dinit > 0.0) && self.d.iter().any(|value| !(*value > 0.0)) {
            m = 18;
        }
        if m != 0 {
            self.code = m;
        }
        self
    }

    pub fn iterations(&self) -> i32 {
        self.iv.niter
    }

    pub fn function_evaluations(&self) -> i32 {
        self.iv.nfcall
    }

    pub fn gradient_evaluations(&self) -> i32 {
        self.iv.ngcall
    }

    pub fn objective(&self) -> f64 {
        self.v.f
    }

    pub fn gradient_norm(&self) -> f64 {
        self.v.dgnorm
    }

    /// One call of the driver: returns 1 or 2 to ask for an evaluation at `x`, or the final
    /// return code (3 or more).
    pub fn advance(&mut self) -> i32 {
        if self.code > 2 && self.code != 12 {
            return self.code;
        }
        if self.derivatives != Derivatives::None {
            self.drmng();
            return self.code;
        }
        // DRMNF and DRMNFB.
        let n = self.n;
        let mut at = match self.code {
            2 => Wrapper::L50,
            _ => Wrapper::L10,
        };
        loop {
            at = match at {
                Wrapper::L10 => {
                    self.drmng();
                    if self.code != 2 {
                        return self.code;
                    }
                    Wrapper::L30
                }
                Wrapper::L30 => {
                    if self.iv.niter == 0 {
                        self.g.fill(0.0);
                    }
                    let mut j = 0;
                    for i in 0..n {
                        let row = &self.l[j..j + i + 1];
                        let k = if self.bounded {
                            self.perm[i] as usize - 1
                        } else {
                            i
                        };
                        self.differences.alpha[k] = dd7tpr(row, row);
                        j += i + 1;
                    }
                    self.iv.ngcall -= 1;
                    self.differences.sgirc = 0;
                    self.fx = self.v.f;
                    Wrapper::L60
                }
                Wrapper::L50 => {
                    if self.iv.toobig != 0 {
                        Wrapper::L10
                    } else {
                        Wrapper::L60
                    }
                }
                Wrapper::L60 => {
                    let differences = &mut self.differences;
                    if self.bounded {
                        ds3grd(
                            &differences.alpha,
                            &self.lower,
                            &self.upper,
                            &self.d,
                            self.v.eta0,
                            &mut self.fx,
                            &mut self.g,
                            &mut differences.sgirc,
                            n,
                            &mut differences.w,
                            &mut self.x,
                        );
                    } else {
                        ds7grd(
                            &differences.alpha,
                            &self.d,
                            self.v.eta0,
                            &mut self.fx,
                            &mut self.g,
                            &mut differences.sgirc,
                            n,
                            &mut differences.w,
                            &mut self.x,
                        );
                    }
                    let i = differences.sgirc;
                    if i == 0 {
                        Wrapper::L10
                    } else if self.bounded && i > n as i32 {
                        self.iv.toobig = 1;
                        Wrapper::L10
                    } else {
                        self.iv.ngcall += 1;
                        self.code = 2;
                        return 2;
                    }
                }
            }
        }
    }

    /// `DRMNG`, `DRMNGB`, `DRMNH` and `DRMNHB`, which share their structure. Labels carry
    /// `DRMNG`'s statement numbers; the bounded and Hessian drivers differ inside them.
    fn drmng(&mut self) {
        let n = self.n;
        let hessian = self.derivatives == Derivatives::Hessian;
        let bounded = self.bounded;
        let mut rstrst = 0;
        let mut at = match self.code {
            1 => Label::L50,
            2 => Label::L60,
            _ => Label::L20,
        };
        loop {
            at = match at {
                Label::L20 => {
                    let iv = &mut self.iv;
                    iv.niter = 0;
                    iv.nfcall = 1;
                    iv.ngcall = 1;
                    iv.nfgcal = 1;
                    iv.mode = -1;
                    iv.model = 1;
                    iv.stglim = 1;
                    iv.toobig = 0;
                    iv.cnvcod = 0;
                    iv.radinc = 0;
                    self.v.rad0 = 0.0;
                    if hessian {
                        self.v.stppar = 0.0;
                    }
                    let mut crossed = false;
                    if bounded {
                        iv.nc = n as i32;
                        for i in 0..n {
                            self.perm[i] = i as i32 + 1;
                            if self.lower[i] > self.upper[i] {
                                crossed = true;
                                break;
                            }
                        }
                    }
                    if crossed {
                        self.code = 82;
                        Label::L350
                    } else {
                        if self.v.dinit >= 0.0 {
                            self.d.fill(self.v.dinit);
                        }
                        if !hessian && self.iv.inith == 1 {
                            self.l.fill(0.0);
                            let mut k = 0;
                            for i in 0..n {
                                k += i + 1;
                                let mut t = self.d[i];
                                if t <= 0.0 {
                                    t = 1.0;
                                }
                                self.l[k - 1] = t;
                            }
                        }
                        self.code = 1;
                        Label::Clamp
                    }
                }
                Label::L50 => {
                    self.v.f = self.fx;
                    if self.iv.mode >= 0 {
                        Label::L190
                    } else {
                        self.v.f0 = self.fx;
                        self.code = 2;
                        if self.iv.toobig == 0 {
                            return;
                        }
                        self.code = 63;
                        Label::L350
                    }
                }
                Label::L60 => {
                    if self.iv.toobig != 0 {
                        self.code = 65;
                        Label::L350
                    } else {
                        Label::L70
                    }
                }
                Label::L70 => {
                    self.take_gradient();
                    if self.iv.cnvcod != 0 {
                        Label::L340
                    } else if self.iv.mode == 0 {
                        Label::L300
                    } else {
                        self.v.radius = if hessian {
                            self.v.lmax0 / (1.0 + self.v.phmxfc)
                        } else {
                            self.v.lmax0
                        };
                        self.iv.mode = 0;
                        Label::L80
                    }
                }
                Label::L80 => {
                    self.ditsum();
                    Label::L90
                }
                Label::L90 => {
                    let k = self.iv.niter;
                    if k >= self.iv.mxiter {
                        self.code = 10;
                        Label::L350
                    } else {
                        self.start_iteration(k);
                        Label::L130
                    }
                }
                Label::L130 => {
                    if self.iv.nfcall < self.iv.mxfcal {
                        Label::L150
                    } else {
                        self.code = 9;
                        if self.v.f >= self.v.f0 {
                            Label::L350
                        } else {
                            self.iv.cnvcod = self.code;
                            Label::L290
                        }
                    }
                }
                Label::L150 => {
                    self.compute_step();
                    if self.iv.irc != 6 {
                        Label::L170
                    } else if self.iv.restor != 2 {
                        Label::L190
                    } else {
                        rstrst = 2;
                        Label::L200
                    }
                }
                Label::L170 => {
                    self.iv.toobig = 0;
                    if self.v.dstnrm <= 0.0 {
                        Label::L190
                    } else if self.iv.irc == 5
                        && self.v.radfac > 1.0
                        && !(self.v.preduc > 1.2 * self.v.fdif)
                    {
                        if self.iv.restor != 2 {
                            Label::L190
                        } else {
                            rstrst = 0;
                            Label::L200
                        }
                    } else {
                        dv2axy(&mut self.x, 1.0, &self.step[..n], &self.x0[..n]);
                        self.iv.nfcall += 1;
                        self.code = 1;
                        Label::Clamp
                    }
                }
                Label::L190 => {
                    rstrst = 3;
                    Label::L200
                }
                Label::L200 => {
                    self.v.reldx = drldst(&self.d, &self.x, &self.x0[..n]);
                    da7sst(&mut self.iv, &mut self.v);
                    let (step, rest) = self.step.split_at_mut(n);
                    let lstgst = &mut rest[n..];
                    match self.iv.restor {
                        1 => self.x.copy_from_slice(&self.x0[..n]),
                        2 if bounded => lstgst.copy_from_slice(&self.x),
                        2 => lstgst.copy_from_slice(step),
                        3 => {
                            if bounded {
                                self.x.copy_from_slice(lstgst);
                                dv2axy(step, -1.0, &self.x0[..n], &self.x);
                            } else {
                                step.copy_from_slice(lstgst);
                                dv2axy(&mut self.x, 1.0, step, &self.x0[..n]);
                            }
                            self.v.reldx = drldst(&self.d, &self.x, &self.x0[..n]);
                            self.iv.restor = rstrst;
                        }
                        _ => {}
                    }
                    let k = self.iv.irc;
                    match k {
                        1 | 5 => {
                            self.v.radius = self.v.radfac * self.v.dstnrm;
                            Label::L130
                        }
                        2..=4 => Label::L280,
                        6 => {
                            self.v.radius = self.v.lmaxs;
                            Label::L150
                        }
                        7..=12 => {
                            self.iv.cnvcod = k - 4;
                            if self.v.f >= self.v.f0 || self.iv.xirc == 14 {
                                Label::L340
                            } else {
                                self.iv.xirc = 14;
                                Label::L280
                            }
                        }
                        13 => {
                            self.code = 64;
                            Label::L350
                        }
                        _ => Label::L300,
                    }
                }
                Label::L280 => {
                    self.prepare_secant_test();
                    Label::L290
                }
                Label::L290 => {
                    if bounded && !hessian {
                        self.nwtstp[n..].copy_from_slice(&self.g);
                    }
                    self.iv.ngcall += 1;
                    if bounded || hessian {
                        self.iv.toobig = 0;
                    }
                    self.code = 2;
                    return;
                }
                Label::L300 => {
                    self.update_model();
                    self.code = 2;
                    Label::L80
                }
                Label::L340 => {
                    self.code = self.iv.cnvcod;
                    self.iv.cnvcod = 0;
                    Label::L350
                }
                Label::L350 => {
                    self.ditsum();
                    return;
                }
                Label::Clamp => {
                    if bounded {
                        for i in 0..n {
                            if self.x[i] < self.lower[i] {
                                self.x[i] = self.lower[i];
                            }
                            if self.x[i] > self.upper[i] {
                                self.x[i] = self.upper[i];
                            }
                        }
                    }
                    return;
                }
            }
        }
    }

    /// What the drivers do with a new gradient before the convergence checks: `DRMNG`'s
    /// statement 70, `DRMNGB`'s 90 (which updates the active set), `DRMNH`'s 50 and
    /// `DRMNHB`'s 70 (which scale the Hessian).
    fn take_gradient(&mut self) {
        let n = self.n;
        match (self.bounded, self.derivatives == Derivatives::Hessian) {
            (false, false) => {
                dv7vmp_divide(&mut self.dg[..n], &self.g, &self.d);
                self.v.dgnorm = dv2nrm(&self.dg[..n]);
            }
            (true, false) => {
                let mut n1 = n;
                let k = n - self.iv.nc as usize;
                for i in 1..=n {
                    let j = self.perm[n - i] as usize - 1;
                    let held = self.lower[j] >= self.upper[j]
                        || (self.x[j] <= self.lower[j] && self.g[j] > 0.0)
                        || (self.x[j] >= self.upper[j] && self.g[j] < 0.0);
                    if !held {
                        if i <= k {
                            self.iv.cnvcod = 0;
                        }
                        continue;
                    }
                    let i1 = n + 1 - i;
                    if i1 < n1 {
                        i7shft(n1, i1, &mut self.perm);
                        dq7rsh(i1, n1, &mut self.l, &mut self.nwtstp[n..]);
                    }
                    n1 -= 1;
                }
                self.iv.nc = n1 as i32;
                self.v.dgnorm = 0.0;
                if n1 > 0 {
                    dv7vmp_divide(&mut self.dg[..n], &self.g, &self.d);
                    dv7ipr(&self.perm, &mut self.dg[..n]);
                    self.v.dgnorm = dv2nrm(&self.dg[..n1]);
                }
            }
            (bounded, true) => {
                dv7vmp_divide(&mut self.dg[..n], &self.g, &self.d);
                if !bounded {
                    self.v.dgnorm = dv2nrm(&self.dg[..n]);
                }
                let mut k = 0;
                for i in 0..n {
                    let t = 1.0 / self.d[i];
                    for j in 0..=i {
                        self.h[k] = t * self.h[k] / self.d[j];
                        k += 1;
                    }
                }
                if bounded {
                    // I7PNVR: the old permutation's inverse, to see which variables come free.
                    for (i, position) in self.perm.iter().enumerate() {
                        self.ipiv1[*position as usize - 1] = i as i32 + 1;
                    }
                    let k = self.iv.nc;
                    let mut ipi = 0;
                    let mut ipn = n;
                    for i in 0..n {
                        let held = self.lower[i] >= self.upper[i]
                            || (self.x[i] <= self.lower[i] && self.g[i] > 0.0)
                            || (self.x[i] >= self.upper[i] && self.g[i] < 0.0);
                        if held {
                            ipn -= 1;
                            self.perm[ipn] = i as i32 + 1;
                        } else {
                            self.perm[ipi] = i as i32 + 1;
                            ipi += 1;
                            if self.ipiv1[i] > k {
                                self.iv.cnvcod = 0;
                            }
                        }
                    }
                    self.iv.nc = ipn as i32;
                    ds7ipr(n, &self.perm, &mut self.h);
                    dv7ipr(&self.perm, &mut self.dg[..n]);
                    self.v.dgnorm = 0.0;
                    if self.iv.nc > 0 {
                        self.v.dgnorm = dv2nrm(&self.dg[..self.iv.nc as usize]);
                    }
                }
            }
        }
    }

    /// The start of an iteration (`DRMNG`'s statement 100): save the point and set the
    /// trust radius from the last step.
    fn start_iteration(&mut self, k: i32) {
        let n = self.n;
        self.iv.niter = k + 1;
        if self.derivatives == Derivatives::Hessian {
            self.v.f0 = self.v.f;
            self.iv.irc = 4;
            self.iv.kagqt = -1;
            self.x0[..n].copy_from_slice(&self.x);
            if k != 0 {
                for i in 0..n {
                    self.step[i] = self.d[i] * self.step[i];
                }
                let t = self.v.radfac * dv2nrm(&self.step[..n]);
                if !self.bounded || self.v.radfac < 1.0 || t > self.v.radius {
                    self.v.radius = t;
                }
            }
        } else if self.bounded {
            if k != 0 {
                let t = self.v.radfac * self.v.dstnrm;
                if self.v.radfac < 1.0 || t > self.v.radius {
                    self.v.radius = t;
                }
            }
            self.v.f0 = self.v.f;
            self.iv.irc = 4;
            self.iv.kagqt = -1;
            self.x0[..n].copy_from_slice(&self.x);
        } else {
            if k > 0 {
                self.v.radius = self.v.radfac * self.v.dstnrm;
            }
            self.v.f0 = self.v.f;
            self.iv.irc = 4;
            self.iv.kagqt = -1;
            self.x0[..n].copy_from_slice(&self.x);
            self.nwtstp[n..].copy_from_slice(&self.g);
        }
    }

    /// The trial step (`DRMNG`'s statement 150).
    fn compute_step(&mut self) {
        let n = self.n;
        match (self.bounded, self.derivatives == Derivatives::Hessian) {
            (false, false) => {
                let (nwtst, _) = self.nwtstp.split_at_mut(n);
                let step = &mut self.step[..n];
                let dg = &mut self.dg[..n];
                if self.iv.kagqt < 0 {
                    nwtst.copy_from_slice(&self.g);
                    dl7ivm(n, nwtst, &self.l);
                    self.v.nreduc = 0.5 * dd7tpr(nwtst, nwtst);
                    dl7itv(n, nwtst, &self.l);
                    dv7vmp_multiply(step, nwtst, &self.d);
                    self.v.dst0 = dv2nrm(step);
                    for i in 0..n {
                        dg[i] = dg[i] / self.d[i];
                    }
                    step.copy_from_slice(dg);
                    dl7tvm(n, step, &self.l);
                    self.v.gthg = dv2nrm(step);
                    self.iv.kagqt = 0;
                }
                dd7dog(dg, n, nwtst, step, &mut self.v);
            }
            (true, false) => {
                let (dig, tg) = self.dg.split_at_mut(n);
                let (step, rest) = self.step.split_at_mut(n);
                let (nwtst, w) = self.nwtstp.split_at_mut(n);
                let (x0, td) = self.x0.split_at_mut(n);
                dd7dgb(
                    &self.lower,
                    &self.upper,
                    &self.d,
                    dig,
                    &mut rest[..n],
                    &self.g,
                    &mut self.perm,
                    &mut self.iv.kagqt,
                    &mut self.l,
                    n,
                    self.iv.nc as usize,
                    nwtst,
                    step,
                    td,
                    tg,
                    &mut self.v,
                    w,
                    x0,
                );
            }
            (false, true) => {
                dg7qts(
                    &self.d,
                    &self.dg[..n],
                    &mut self.h,
                    &mut self.iv.kagqt,
                    &mut self.l,
                    n,
                    &mut self.step[..n],
                    &mut self.v,
                    &mut self.work,
                );
            }
            (true, true) => {
                let (tg, td) = self.dg.split_at_mut(n);
                let (x0, x) = self.x0.split_at_mut(n);
                dg7qsb(
                    &self.lower,
                    &self.upper,
                    &self.d,
                    &mut self.h,
                    &self.g,
                    &mut self.perm,
                    &mut self.ipiv1,
                    &mut self.ipiv2,
                    &mut self.iv.kagqt,
                    &mut self.l,
                    n,
                    &mut self.iv.n0,
                    self.iv.nc as usize,
                    &mut self.step[..2 * n],
                    td,
                    tg,
                    &mut self.v,
                    &mut self.work,
                    x,
                    x0,
                );
            }
        }
    }

    /// `DRMNG`'s statement 280: before asking for the gradient after an accepted step, the
    /// model's prediction of the new gradient for the test that may grow the radius.
    fn prepare_secant_test(&mut self) {
        let n = self.n;
        let hessian = self.derivatives == Derivatives::Hessian;
        let (step, rest) = self.step.split_at_mut(n);
        let lstgst = &mut rest[n..];
        if self.bounded && !hessian {
            dv2axy(step, -1.0, &self.x0[..n], &self.x);
        }
        if self.iv.irc != 3 {
            return;
        }
        match (self.bounded, hessian) {
            (false, false) => {
                lstgst.copy_from_slice(step);
                dl7tvm(n, lstgst, &self.l);
                dl7vml(n, lstgst, &self.l);
            }
            (true, false) => {
                let x0 = &mut self.x0[..n];
                x0.copy_from_slice(step);
                dv7ipr(&self.perm, x0);
                dl7tvm(n, x0, &self.l);
                dl7vml(n, x0, &self.l);
                for i in 0..n {
                    lstgst[self.perm[i] as usize - 1] = x0[i];
                }
            }
            (false, true) => {
                let x0 = &mut self.x0[..n];
                for i in 0..n {
                    x0[i] = self.d[i] * step[i];
                }
                ds7lvm(n, lstgst, &self.h, x0);
                for i in 0..n {
                    lstgst[i] = self.d[i] * lstgst[i] + self.g[i];
                }
            }
            (true, true) => {
                for i in 0..n {
                    let j = self.perm[i] as usize - 1;
                    lstgst[i] = self.d[j] * step[j];
                }
                let x0 = &mut self.x0[..n];
                ds7lvm(n, x0, &self.h, lstgst);
                for i in 0..n {
                    let j = self.perm[i] as usize - 1;
                    lstgst[j] = self.d[j] * x0[i] + self.g[j];
                }
            }
        }
    }

    /// `DRMNG`'s statement 300, after the gradient at an accepted point: the test that may
    /// grow the radius, then for the secant drivers the BFGS update of `L`.
    fn update_model(&mut self) {
        let n = self.n;
        let hessian = self.derivatives == Derivatives::Hessian;
        let (step, rest) = self.step.split_at_mut(n);
        let lstgst = &mut rest[n..];
        let (w, g0) = self.nwtstp.split_at_mut(n);
        if !hessian {
            for i in 0..n {
                g0[i] = -1.0 * g0[i] + self.g[i];
            }
        }
        if self.iv.irc == 3 {
            for i in 0..n {
                lstgst[i] = if hessian {
                    (lstgst[i] - self.g[i]) / self.d[i]
                } else {
                    (-1.0 * g0[i] + lstgst[i]) / self.d[i]
                };
            }
            if dv2nrm(lstgst) <= self.v.dgnorm * self.v.tuner4
                || !(dd7tpr(&self.g, step) >= self.v.gtstep * self.v.tuner5)
            {
                self.v.radfac = self.v.incfac;
            }
        }
        if hessian {
            return;
        }
        if self.bounded {
            dv7ipr(&self.perm, step);
            dv7ipr(&self.perm, g0);
        }
        let z = &mut self.x0[..n];
        dw7zbf(&self.l, n, step, w, g0, z);
        dl7upd(lstgst, step, &mut self.l, g0, n, w, z);
    }

    /// R's replacement for PORT's `DITSUM`: with `trace` nonzero, every `trace`-th
    /// iteration prints its number, the objective and the point.
    fn ditsum(&self) {
        if self.trace != 0 && self.iv.niter % self.trace == 0 {
            print_trace(self.iv.niter, self.v.f, &self.x);
        }
    }
}

/// One line of `nlminb`'s trace, in GNU R's `"%3d:%#14.8g:"` and `" %#8g"` layout.
pub fn print_trace(iteration: i32, value: f64, x: &[f64]) {
    let mut line = format!("{iteration:>3}:{:>14}:", c_general(value, 8, true));
    for entry in x {
        line.push_str(&format!(" {:>8}", c_general(*entry, 6, true)));
    }
    line.push('\n');
    write_console(line.as_bytes(), false);
}

/// PORT's `IV` slots that the minimizers use.
#[derive(Default)]
struct Counters {
    cnvcod: i32,
    inith: i32,
    irc: i32,
    kagqt: i32,
    mlstgd: i32,
    mode: i32,
    model: i32,
    mxfcal: i32,
    mxiter: i32,
    n0: i32,
    nc: i32,
    nfcall: i32,
    nfgcal: i32,
    ngcall: i32,
    niter: i32,
    radinc: i32,
    restor: i32,
    stage: i32,
    stglim: i32,
    switch: i32,
    toobig: i32,
    xirc: i32,
}

/// PORT's `V` slots that the minimizers use.
#[derive(Default)]
struct Scalars {
    dgnorm: f64,
    dstnrm: f64,
    dst0: f64,
    gtstep: f64,
    stppar: f64,
    nreduc: f64,
    preduc: f64,
    radius: f64,
    rad0: f64,
    f: f64,
    fdif: f64,
    flstgd: f64,
    f0: f64,
    gtslst: f64,
    plstgd: f64,
    radfac: f64,
    reldx: f64,
    dstsav: f64,
    gthg: f64,
    grdfac: f64,
    nwtfac: f64,
    epslon: f64,
    phmnfc: f64,
    phmxfc: f64,
    decfac: f64,
    incfac: f64,
    rdfcmn: f64,
    rdfcmx: f64,
    tuner1: f64,
    tuner2: f64,
    tuner3: f64,
    tuner4: f64,
    tuner5: f64,
    afctol: f64,
    rfctol: f64,
    xctol: f64,
    xftol: f64,
    lmax0: f64,
    lmaxs: f64,
    sctol: f64,
    dinit: f64,
    eta0: f64,
    bias: f64,
}

/// The finite-difference drivers' own state: `ALPHA`, the curvature estimates that set the
/// step sizes, and `DS7GRD`'s work vector and return code.
struct Differences {
    alpha: Vec<f64>,
    w: [f64; 6],
    sgirc: i32,
}

#[derive(Clone, Copy)]
enum Wrapper {
    L10,
    L30,
    L50,
    L60,
}

#[derive(Clone, Copy)]
enum Label {
    L20,
    L50,
    L60,
    L70,
    L80,
    L90,
    L130,
    L150,
    L170,
    L190,
    L200,
    L280,
    L290,
    L300,
    L340,
    L350,
    Clamp,
}

/// `DA7SST`: assess the trial step, decide whether to accept it, how to change the trust
/// radius, and whether the run has converged; the outcome is `IV(IRC)`.
fn da7sst(iv: &mut Counters, v: &mut Scalars) {
    #[derive(Clone, Copy)]
    enum At {
        L10,
        L20,
        L30,
        L40,
        L50,
        L60,
        L70,
        L80,
        L110,
        L140,
        L160,
        L170,
        L190,
        L200,
        L210,
        L220,
        L230,
        L240,
        L250,
        L260,
        L270,
        L280,
        L290,
    }
    let mut nfc = iv.nfcall;
    iv.switch = 0;
    iv.restor = 0;
    let mut rfac1 = 1.0;
    let mut goodx = true;
    let mut emaxs = 0.0;
    let mut i = iv.irc;
    let mut at = match i {
        1 => At::L20,
        2 => At::L30,
        3 | 4 => At::L10,
        5 => At::L40,
        6 => At::L280,
        7..=11 => At::L220,
        12 => At::L170,
        _ => {
            iv.irc = 13;
            return;
        }
    };
    loop {
        at = match at {
            At::L10 => {
                iv.stage = 1;
                iv.radinc = 0;
                v.flstgd = v.f0;
                if iv.toobig == 0 {
                    At::L110
                } else {
                    iv.stage = -1;
                    iv.xirc = i;
                    At::L60
                }
            }
            At::L20 => {
                if iv.model != iv.mlstgd {
                    At::L30
                } else {
                    iv.stage = iv.stglim;
                    iv.radinc = -1;
                    At::L110
                }
            }
            At::L30 => {
                iv.stage += 1;
                At::L40
            }
            At::L40 => {
                if iv.stage > 0 {
                    At::L50
                } else if iv.toobig != 0 {
                    At::L60
                } else {
                    iv.stage = -iv.stage;
                    i = iv.xirc;
                    match i {
                        1 => At::L20,
                        2 => At::L30,
                        3 | 4 => At::L110,
                        5 => At::L70,
                        _ => At::L50,
                    }
                }
            }
            At::L50 => {
                if iv.toobig == 0 {
                    At::L70
                } else {
                    iv.toobig = 0;
                    if iv.radinc > 0 {
                        At::L80
                    } else {
                        iv.stage = -iv.stage;
                        iv.xirc = iv.irc;
                        At::L60
                    }
                }
            }
            At::L60 => {
                iv.toobig = 0;
                v.radfac = v.decfac;
                iv.radinc -= 1;
                iv.irc = 5;
                iv.restor = 1;
                v.f = v.flstgd;
                return;
            }
            At::L70 => {
                if v.f < v.flstgd {
                    At::L110
                } else {
                    if iv.model != iv.mlstgd {
                        iv.model = iv.mlstgd;
                        iv.switch = 1;
                    }
                    At::L80
                }
            }
            At::L80 => {
                if v.flstgd >= v.f0 {
                    At::L110
                } else {
                    if iv.stage < iv.stglim
                        || nfc < iv.nfgcal + iv.stglim + 2
                        || iv.switch != 0
                    {
                        goodx = false;
                    }
                    iv.restor = 3;
                    v.f = v.flstgd;
                    v.preduc = v.plstgd;
                    v.gtstep = v.gtslst;
                    if iv.switch == 0 {
                        rfac1 = v.dstnrm / v.dstsav;
                    }
                    v.dstnrm = v.dstsav;
                    if goodx {
                        v.fdif = v.f0 - v.f;
                        iv.irc = 4;
                        v.radfac = rfac1;
                        return;
                    }
                    nfc = iv.nfgcal;
                    At::L110
                }
            }
            At::L110 => {
                v.fdif = v.f0 - v.f;
                if v.fdif > v.tuner2 * v.preduc || iv.radinc > 0 {
                    At::L140
                } else {
                    if v.f < v.f0 {
                        iv.nfgcal = nfc;
                    } else {
                        iv.mlstgd = iv.model;
                        v.flstgd = v.f;
                        v.f = v.f0;
                        iv.restor = 1;
                    }
                    iv.irc = 1;
                    if iv.stage >= iv.stglim {
                        iv.irc = 5;
                        iv.radinc -= 1;
                    }
                    At::L160
                }
            }
            At::L140 => {
                iv.nfgcal = nfc;
                rfac1 = 1.0;
                v.dstsav = v.dstnrm;
                if v.fdif > v.preduc * v.tuner1 {
                    At::L190
                } else {
                    iv.irc = if iv.stage >= iv.stglim { 4 } else { 2 };
                    At::L160
                }
            }
            At::L160 => {
                iv.xirc = iv.irc;
                let emax = v.gtstep + v.fdif;
                v.radfac = 0.5 * rfac1;
                if emax < v.gtstep {
                    v.radfac = rfac1 * dmax1(v.rdfcmn, 0.5 * v.gtstep / emax);
                }
                At::L170
            }
            At::L170 => {
                if v.reldx <= v.xftol {
                    iv.irc = 12;
                    At::L240
                } else {
                    iv.irc = iv.xirc;
                    if v.f < v.f0 { At::L200 } else { At::L230 }
                }
            }
            At::L190 => {
                if v.fdif < -v.tuner3 * v.gtstep
                    || iv.radinc < 0
                    || iv.restor == 1
                    || iv.restor == 3
                {
                    At::L210
                } else {
                    v.radfac = v.rdfcmx;
                    let gts = v.gtstep;
                    if v.fdif < (0.5 / v.radfac - 1.0) * gts {
                        v.radfac = dmax1(v.incfac, 0.5 * gts / (gts + v.fdif));
                    }
                    iv.irc = 4;
                    if v.stppar == 0.0
                        || (v.dst0 >= 0.0
                            && (v.dst0 < 2.0 * v.dstnrm || v.nreduc < 1.2 * v.fdif))
                    {
                        At::L230
                    } else {
                        iv.irc = 5;
                        iv.radinc += 1;
                        At::L200
                    }
                }
            }
            At::L200 => {
                v.flstgd = v.f;
                iv.mlstgd = iv.model;
                if iv.restor == 0 {
                    iv.restor = 2;
                }
                v.dstsav = v.dstnrm;
                iv.nfgcal = nfc;
                v.plstgd = v.preduc;
                v.gtslst = v.gtstep;
                At::L230
            }
            At::L210 => {
                v.radfac = 1.0;
                iv.irc = 3;
                At::L230
            }
            At::L220 => {
                iv.irc = iv.xirc;
                if !(v.dstsav >= 0.0) {
                    iv.irc = 12;
                }
                At::L240
            }
            At::L230 => {
                iv.xirc = iv.irc;
                At::L240
            }
            At::L240 => {
                if iv.restor == 1 && v.flstgd < v.f0 {
                    iv.restor = 3;
                }
                if v.f.abs() < v.afctol {
                    iv.irc = 10;
                }
                if 0.5 * v.fdif > v.preduc {
                    return;
                }
                let emax = v.rfctol * v.f0.abs();
                emaxs = v.sctol * v.f0.abs();
                if v.preduc <= emaxs && (v.dstnrm > v.lmaxs || v.stppar == 0.0) {
                    iv.irc = 11;
                }
                if !(v.dst0 < 0.0) {
                    let mut i = 0;
                    if (v.nreduc > 0.0 && v.nreduc <= emax)
                        || (v.nreduc == 0.0 && v.preduc == 0.0)
                    {
                        i = 2;
                    }
                    if v.stppar == 0.0 && v.reldx <= v.xctol && goodx {
                        i += 1;
                    }
                    if i > 0 {
                        iv.irc = i + 6;
                    }
                }
                At::L250
            }
            At::L250 => {
                if (iv.irc > 5 && iv.irc != 12) || v.stppar == 0.0 {
                    return;
                }
                if v.dstnrm > v.lmaxs {
                    At::L260
                } else if v.preduc >= emaxs {
                    return;
                } else if v.dst0 <= 0.0 {
                    At::L270
                } else if 0.5 * v.dst0 <= v.lmaxs {
                    return;
                } else {
                    At::L270
                }
            }
            At::L260 => {
                if 0.5 * v.dstnrm <= v.lmaxs {
                    return;
                }
                let xmax = v.lmaxs / v.dstnrm;
                if xmax * (2.0 - xmax) * v.preduc >= emaxs {
                    return;
                }
                At::L270
            }
            At::L270 => {
                if v.nreduc < 0.0 {
                    At::L290
                } else {
                    v.gtslst = v.gtstep;
                    v.dstsav = v.dstnrm;
                    if iv.irc == 12 {
                        v.dstsav = -v.dstsav;
                    }
                    v.plstgd = v.preduc;
                    let i = iv.restor;
                    iv.restor = 2;
                    if i == 3 {
                        iv.restor = 0;
                    }
                    iv.irc = 6;
                    return;
                }
            }
            At::L280 => {
                v.gtstep = v.gtslst;
                v.dstnrm = v.dstsav.abs();
                iv.irc = iv.xirc;
                if v.dstsav <= 0.0 {
                    iv.irc = 12;
                }
                v.nreduc = -v.preduc;
                v.preduc = v.plstgd;
                iv.restor = 3;
                At::L290
            }
            At::L290 => {
                if -v.nreduc <= v.sctol * v.f0.abs() {
                    iv.irc = 11;
                }
                return;
            }
        }
    }
}

/// `DD7DOG`: the double-dogleg step, a combination of the steepest-descent and Newton
/// steps of length at most the trust radius.
fn dd7dog(dig: &[f64], n: usize, nwtstp: &[f64], step: &mut [f64], v: &mut Scalars) {
    let nwtnrm = v.dst0;
    let mut rlambd = 1.0;
    if nwtnrm > 0.0 {
        rlambd = v.radius / nwtnrm;
    }
    let gnorm = v.dgnorm;
    let ghinvg = 2.0 * v.nreduc;
    v.grdfac = 0.0;
    v.nwtfac = 0.0;
    if rlambd >= 1.0 {
        v.stppar = 0.0;
        v.dstnrm = nwtnrm;
        v.gtstep = -ghinvg;
        v.preduc = v.nreduc;
        v.nwtfac = -1.0;
        for i in 0..n {
            step[i] = -nwtstp[i];
        }
        return;
    }
    v.dstnrm = v.radius;
    let cfact = square(gnorm / v.gthg);
    let cnorm = gnorm * cfact;
    let relax = 1.0 - v.bias * (1.0 - gnorm * cnorm / ghinvg);
    if rlambd >= relax {
        v.stppar = 1.0 - (rlambd - relax) / (1.0 - relax);
        let t = -rlambd;
        v.gtstep = t * ghinvg;
        v.preduc = rlambd * (1.0 - 0.5 * rlambd) * ghinvg;
        v.nwtfac = t;
        for i in 0..n {
            step[i] = t * nwtstp[i];
        }
        return;
    }
    if cnorm >= v.radius {
        let t = -v.radius / gnorm;
        v.grdfac = t;
        v.stppar = 1.0 + cnorm / v.radius;
        v.gtstep = -v.radius * gnorm;
        v.preduc = v.radius * (gnorm - 0.5 * v.radius * square(v.gthg / gnorm));
        for i in 0..n {
            step[i] = t * dig[i];
        }
        return;
    }
    let ctrnwt = cfact * relax * ghinvg / gnorm;
    let mut t1 = ctrnwt - gnorm * square(cfact);
    let mut t2 = v.radius * (v.radius / gnorm) - gnorm * square(cfact);
    let mut t = relax * nwtnrm;
    let femnsq = (t / gnorm) * t - ctrnwt - t1;
    t = t2 / (t1 + (square(t1) + femnsq * t2).sqrt());
    t1 = (t - 1.0) * cfact;
    v.grdfac = t1;
    t2 = -t * relax;
    v.nwtfac = t2;
    v.stppar = 2.0 - t;
    v.gtstep = t1 * square(gnorm) + t2 * ghinvg;
    v.preduc = -t1 * gnorm * ((t2 + 1.0) * gnorm)
        - t2 * (1.0 + 0.5 * t2) * ghinvg
        - 0.5 * square(v.gthg * t1);
    for i in 0..n {
        step[i] = t1 * dig[i] + t2 * nwtstp[i];
    }
}

/// `DD7DGB`: the double-dogleg step under bounds, which restarts the dogleg in the
/// remaining free variables each time the step reaches a bound.
#[allow(clippy::too_many_arguments)]
fn dd7dgb(
    lower: &[f64],
    upper: &[f64],
    d: &[f64],
    dig: &mut [f64],
    dst: &mut [f64],
    g: &[f64],
    ipiv: &mut [i32],
    ka: &mut i32,
    l: &mut [f64],
    p: usize,
    pc: usize,
    nwtst: &mut [f64],
    step: &mut [f64],
    td: &mut [f64],
    tg: &mut [f64],
    v: &mut Scalars,
    w: &mut [f64],
    x0: &[f64],
) {
    let meps2 = 2.0 * f64::EPSILON;
    let gnorm0 = v.dgnorm;
    v.dstnrm = 0.0;
    let mut dnwtst = 0.0;
    // PORT leaves NRED unset when no variable is free. Any value other than zero then gives
    // the reference's outcome, X-convergence; NaN keeps it from reading as a reduction.
    let mut nred = f64::NAN;
    if *ka >= 0 {
        dnwtst = v.dst0;
        nred = v.nreduc;
    }
    let mut pred = 0.0;
    v.stppar = 0.0;
    let rad = v.radius;
    if pc == 0 {
        dnwtst = 0.0;
        step[..p].fill(0.0);
    } else {
        let mut p1 = pc;
        td[..p].copy_from_slice(&d[..p]);
        dv7ipr(ipiv, &mut td[..p]);
        dst[..pc].fill(0.0);
        tg[..p].copy_from_slice(&g[..p]);
        dv7ipr(ipiv, &mut tg[..p]);
        loop {
            nwtst[..p1].copy_from_slice(&tg[..p1]);
            dl7ivm(p1, nwtst, l);
            let ghinvg = dd7tpr(&nwtst[..p1], &nwtst[..p1]);
            v.nreduc = 0.5 * ghinvg;
            dl7itv(p1, nwtst, l);
            for i in 0..p1 {
                step[i] = nwtst[i] * td[i];
            }
            v.dst0 = dv2nrm(&step[..pc]);
            if *ka < 0 {
                *ka = 0;
                dnwtst = v.dst0;
                nred = v.nreduc;
            }
            v.radius = rad - v.dstnrm;
            if v.radius <= 0.0 {
                break;
            }
            for i in 0..p1 {
                dig[i] = tg[i] / td[i];
            }
            let gnorm = dv2nrm(&dig[..p1]);
            if gnorm <= 0.0 {
                break;
            }
            v.dgnorm = gnorm;
            for i in 0..p1 {
                dig[i] = dig[i] / td[i];
            }
            w[..p1].copy_from_slice(&dig[..p1]);
            dl7tvm(p1, w, l);
            v.gthg = dv2nrm(&w[..p1]);
            *ka += 1;
            dd7dog(dig, p1, nwtst, step, v);
            let mut t = 1.0;
            let mut k: i64 = 0;
            for i in 1..=p1 {
                let j = ipiv[i - 1] as usize;
                let x0i = x0[j - 1] + dst[i - 1] / td[i - 1];
                let xi = x0i + step[i - 1];
                let (ti, candidate) = if xi < lower[j - 1] {
                    ((lower[j - 1] - x0i) / step[i - 1], -(i as i64))
                } else if xi <= upper[j - 1] {
                    continue;
                } else {
                    ((upper[j - 1] - x0i) / step[i - 1], i as i64)
                };
                if t <= ti {
                    continue;
                }
                k = candidate;
                t = ti;
            }
            for i in 0..p1 {
                step[i] = step[i] * td[i];
            }
            for i in 0..p1 {
                dst[i] = t * step[i] + dst[i];
            }
            v.dstnrm = dv2nrm(&dst[..pc]);
            let t1 = t * v.grdfac;
            let mut t2 = t * v.nwtfac;
            pred = pred
                - t1 * gnorm * ((t2 + 1.0) * gnorm)
                - t2 * (1.0 + 0.5 * t2) * ghinvg
                - 0.5 * square(v.gthg * t1);
            if k == 0 {
                break;
            }
            dl7vml(p1, w, l);
            t2 = 1.0 - t2;
            for i in 0..p1 {
                tg[i] = t2 * tg[i] - t1 * w[i];
            }
            let p1m1 = p1 - 1;
            let j = k.unsigned_abs() as usize;
            if j != p1 {
                dq7rsh(j, p1, l, w);
                i7shft(p1, j, ipiv);
                dv7shf(p1, j, tg);
                dv7shf(p1, j, td);
                dv7shf(p1, j, dst);
            }
            if k < 0 {
                ipiv[p1 - 1] = -ipiv[p1 - 1];
            }
            p1 = p1m1;
            if p1 == 0 {
                break;
            }
        }
        // Statement 100.
        step[..p].fill(0.0);
        for i in 0..pc {
            let j = ipiv[i].unsigned_abs() as usize;
            step[j - 1] = dst[i] / td[i];
        }
        if p1 < pc {
            for i in 0..p {
                td[i] = 1.0 * step[i] + x0[i];
            }
            for i in p1 + 1..=pc {
                let mut j = ipiv[i - 1];
                let mut t = meps2;
                if j <= 0 {
                    t = -t;
                    j = -j;
                    ipiv[i - 1] = j;
                }
                let j = j as usize;
                t = t * dmax1(td[j - 1].abs(), x0[j - 1].abs());
                step[j - 1] = step[j - 1] + t;
            }
        }
    }
    v.dgnorm = gnorm0;
    v.nreduc = nred;
    v.preduc = pred;
    v.radius = rad;
    v.dst0 = dnwtst;
    v.gtstep = dd7tpr(&step[..p], &g[..p]);
}

/// `DG7QTS`: the More-Hebden step, which solves `(H + alpha D^2) s = -g` for the `alpha`
/// that puts the step on the trust region's boundary.
#[allow(clippy::too_many_arguments)]
fn dg7qts(
    d: &[f64],
    dig: &[f64],
    dihdi: &mut [f64],
    ka: &mut i32,
    l: &mut [f64],
    p: usize,
    step: &mut [f64],
    v: &mut Scalars,
    w: &mut [f64],
) {
    #[derive(Clone, Copy)]
    enum At {
        L30,
        L50,
        L60,
        L70,
        L210,
        L240,
        L250,
        L260,
        L270,
        L290,
        L310,
        L330,
        L340,
        L370,
        L380,
        L410,
    }
    const EPSFAC: f64 = 50.0;
    const KAPPA: f64 = 2.0;
    const P001: f64 = 1.0e-3;
    let big = f64::MAX;
    let dgxfac = EPSFAC * f64::EPSILON;
    // 1-based offsets into W, as in the reference.
    let dggdmx = p + 1;
    let emax = dggdmx + 1;
    let emin = emax + 1;
    let lk0 = emin + 1;
    let phipin = lk0 + 1;
    let uk0 = phipin + 1;
    let dstsav = uk0 + 1;
    let diag0 = dstsav;
    let diag = diag0 + 1;
    let q0 = diag0 + p;
    let q = q0 + 1;
    let x = q + p;
    let rad = v.radius;
    let radsq = square(rad);
    let phimax = v.phmxfc * rad;
    let phimin = v.phmnfc * rad;
    let mut psifac = big;
    let mut t1 = 2.0 * v.epslon
        / (3.0 * (4.0 * (v.phmnfc + 1.0) * (KAPPA + 1.0) + KAPPA + 2.0) * rad);
    if t1 < big * dmin1(rad, 1.0) {
        psifac = t1 / rad;
    }
    let mut oldphi = 0.0;
    let eps = v.epslon;
    let mut irc = 0;
    let mut restrt = false;
    let mut kalim = *ka + 50;
    let mut k;
    let mut uk = 0.0;
    let mut lk = 0.0;
    let mut kamin = 0;
    let mut alphak = 0.0;
    let mut phi = 0.0;
    let mut dst = 0.0;
    let mut gtsta = 0.0;
    let mut delta = 0.0;
    let mut twopsi = 0.0;
    let mut sw = 0.0;
    let mut si = 0.0;
    let mut t = 0.0;
    let mut t2;
    let mut at = if *ka >= 0 { At::L290 } else { At::L30 };
    if *ka < 0 {
        uk = -1.0;
        *ka = 0;
        kalim = 50;
        v.dgnorm = dv2nrm(&dig[..p]);
        v.nreduc = 0.0;
        v.dst0 = 0.0;
        kamin = 3;
        if v.dgnorm == 0.0 {
            kamin = 0;
        }
        let mut j = 0;
        for i in 1..=p {
            j += i;
            w[diag0 + i - 1] = dihdi[j - 1];
        }
        t1 = 0.0;
        for entry in &dihdi[..p * (p + 1) / 2] {
            let t = entry.abs();
            if t1 < t {
                t1 = t;
            }
        }
        w[dggdmx - 1] = t1;
    }
    loop {
        at = match at {
            At::L30 => {
                irc = dl7srt(p, l, dihdi);
                if irc == 0 {
                    At::L50
                } else {
                    let j = irc * (irc + 1) / 2;
                    t = l[j - 1];
                    l[j - 1] = 1.0;
                    w[..irc].fill(0.0);
                    w[irc - 1] = 1.0;
                    dl7itv(irc, w, l);
                    t1 = dv2nrm(&w[..irc]);
                    lk = -t / t1 / t1;
                    v.dst0 = -lk;
                    if restrt { At::L210 } else { At::L70 }
                }
            }
            At::L50 => {
                lk = 0.0;
                let mut scratch = vec![0.0; p];
                t = dl7svn(p, l, &mut scratch, &mut w[q - 1..q - 1 + p]);
                if t >= 1.0 || !(v.dgnorm >= t * t * big) {
                    At::L60
                } else {
                    At::L70
                }
            }
            At::L60 => {
                w[q - 1..q - 1 + p].copy_from_slice(&dig[..p]);
                dl7ivm(p, &mut w[q - 1..q - 1 + p], l);
                gtsta = dd7tpr(&w[q - 1..q - 1 + p], &w[q - 1..q - 1 + p]);
                v.nreduc = 0.5 * gtsta;
                dl7itv(p, &mut w[q - 1..q - 1 + p], l);
                dst = dv2nrm(&w[q - 1..q - 1 + p]);
                v.dst0 = dst;
                phi = dst - rad;
                if phi <= phimax {
                    At::L260
                } else if restrt {
                    At::L210
                } else {
                    At::L70
                }
            }
            At::L70 => {
                k = 0;
                for i in 1..=p {
                    let mut wi = 0.0;
                    for j in 1..i {
                        k += 1;
                        let t = dihdi[k - 1].abs();
                        wi += t;
                        w[j - 1] += t;
                    }
                    w[i - 1] = wi;
                    k += 1;
                }
                k = 1;
                t1 = w[diag - 1] - w[0];
                for i in 2..=p {
                    let j = diag0 + i;
                    let t = w[j - 1] - w[i - 1];
                    if t < t1 {
                        t1 = t;
                        k = i;
                    }
                }
                let sk = w[k - 1];
                let akk = w[diag0 + k - 1];
                let mut k1 = k * (k - 1) / 2 + 1;
                let mut inc = 1;
                t = 0.0;
                for i in 1..=p {
                    if i != k {
                        let aki = dihdi[k1 - 1].abs();
                        let si = w[i - 1];
                        let j = diag0 + i;
                        t1 = 0.5 * (akk - w[j - 1] + si - aki);
                        t1 = t1 + (t1 * t1 + sk * aki).sqrt();
                        if t < t1 {
                            t = t1;
                        }
                        if i < k {
                            k1 += inc;
                            continue;
                        }
                    }
                    inc = i;
                    k1 += inc;
                }
                w[emin - 1] = akk - t;
                uk = v.dgnorm / rad - w[emin - 1];
                if v.dgnorm == 0.0 {
                    uk = uk + P001 + P001 * uk;
                }
                if uk <= 0.0 {
                    uk = P001;
                }
                k = 1;
                t1 = w[diag - 1] + w[0];
                for i in 2..=p {
                    let j = diag0 + i;
                    let t = w[j - 1] + w[i - 1];
                    if t > t1 {
                        t1 = t;
                        k = i;
                    }
                }
                let sk = w[k - 1];
                let akk = w[diag0 + k - 1];
                let mut k1 = k * (k - 1) / 2 + 1;
                let mut inc = 1;
                t = 0.0;
                for i in 1..=p {
                    if i != k {
                        let aki = dihdi[k1 - 1].abs();
                        let si = w[i - 1];
                        let j = diag0 + i;
                        t1 = 0.5 * (w[j - 1] + si - aki - akk);
                        t1 = t1 + (t1 * t1 + sk * aki).sqrt();
                        if t < t1 {
                            t = t1;
                        }
                        if i < k {
                            k1 += inc;
                            continue;
                        }
                    }
                    inc = i;
                    k1 += inc;
                }
                w[emax - 1] = akk + t;
                lk = dmax1(lk, v.dgnorm / rad - w[emax - 1]);
                alphak = v.stppar.abs() * v.rad0 / rad;
                alphak = dmin1(uk, dmax1(alphak, lk));
                if irc != 0 {
                    At::L210
                } else {
                    w.copy_within(q - 1..q - 1 + p, 0);
                    dl7ivm(p, &mut w[..p], l);
                    t = dv2nrm(&w[..p]);
                    w[phipin - 1] = rad / t / t;
                    lk = dmax1(lk, phi * w[phipin - 1]);
                    At::L210
                }
            }
            At::L210 => {
                *ka += 1;
                if -v.dst0 >= alphak || alphak < lk || alphak >= uk {
                    alphak = uk * dmax1(P001, (lk / uk).sqrt());
                }
                if alphak <= 0.0 {
                    alphak = 0.5 * uk;
                }
                if alphak <= 0.0 {
                    alphak = uk;
                }
                let mut k = 0;
                for i in 1..=p {
                    k += i;
                    dihdi[k - 1] = w[diag0 + i - 1] + alphak;
                }
                irc = dl7srt(p, l, dihdi);
                if irc == 0 {
                    At::L240
                } else {
                    let j = irc * (irc + 1) / 2;
                    t = l[j - 1];
                    l[j - 1] = 1.0;
                    w[..irc].fill(0.0);
                    w[irc - 1] = 1.0;
                    dl7itv(irc, w, l);
                    t1 = dv2nrm(&w[..irc]);
                    lk = alphak - t / t1 / t1;
                    v.dst0 = -lk;
                    if uk < lk {
                        uk = lk;
                    }
                    if alphak >= lk {
                        t = P001 * alphak;
                        if t <= 0.0 {
                            t = P001;
                        }
                        lk = alphak + t;
                        if uk <= lk {
                            uk = lk + t;
                        }
                    }
                    At::L210
                }
            }
            At::L240 => {
                w[q - 1..q - 1 + p].copy_from_slice(&dig[..p]);
                dl7ivm(p, &mut w[q - 1..q - 1 + p], l);
                gtsta = dd7tpr(&w[q - 1..q - 1 + p], &w[q - 1..q - 1 + p]);
                dl7itv(p, &mut w[q - 1..q - 1 + p], l);
                dst = dv2nrm(&w[q - 1..q - 1 + p]);
                phi = dst - rad;
                if (phi <= phimax && phi >= phimin) || phi == oldphi {
                    At::L270
                } else {
                    oldphi = phi;
                    if phi < 0.0 { At::L330 } else { At::L250 }
                }
            }
            At::L250 => {
                if *ka >= kalim {
                    At::L270
                } else {
                    if phi < 0.0 {
                        uk = dmin1(uk, alphak);
                    }
                    if kamin == 0 {
                        At::L210
                    } else {
                        w.copy_within(q - 1..q - 1 + p, 0);
                        dl7ivm(p, &mut w[..p], l);
                        t1 = dv2nrm(&w[..p]);
                        alphak = alphak + (phi / t1) * (dst / t1) * (dst / rad);
                        lk = dmax1(lk, alphak);
                        alphak = lk;
                        At::L210
                    }
                }
            }
            At::L260 => {
                alphak = 0.0;
                At::L270
            }
            At::L270 => {
                for i in 1..=p {
                    step[i - 1] = -w[q0 + i - 1] / d[i - 1];
                }
                v.gtstep = -gtsta;
                v.preduc = 0.5 * (alphak.abs() * dst * dst + gtsta);
                At::L410
            }
            At::L290 => {
                if v.dst0 <= 0.0 || v.dst0 - rad > phimax {
                    At::L310
                } else {
                    restrt = true;
                    *ka += 1;
                    let mut k = 0;
                    for i in 1..=p {
                        k += i;
                        dihdi[k - 1] = w[diag0 + i - 1];
                    }
                    uk = -1.0;
                    At::L30
                }
            }
            At::L310 => {
                kamin = *ka + 3;
                if v.dgnorm == 0.0 {
                    kamin = 0;
                }
                if *ka == 0 {
                    At::L50
                } else {
                    dst = w[dstsav - 1];
                    alphak = v.stppar.abs();
                    phi = dst - rad;
                    t = v.dgnorm / rad;
                    uk = t - w[emin - 1];
                    if v.dgnorm == 0.0 {
                        uk = uk + P001 + P001 * uk;
                    }
                    if uk <= 0.0 {
                        uk = P001;
                    }
                    if rad > v.rad0 {
                        if alphak > 0.0 {
                            uk = dmin1(uk, w[uk0 - 1]);
                        }
                        lk = dmax1(dmax1(0.0, -v.dst0), t - w[emax - 1]);
                    } else {
                        lk = 0.0;
                        if alphak > 0.0 {
                            lk = w[lk0 - 1];
                        }
                        lk = dmax1(lk, t - w[emax - 1]);
                    }
                    if v.dst0 > 0.0 {
                        lk = dmax1(lk, (v.dst0 - rad) * w[phipin - 1]);
                    }
                    At::L250
                }
            }
            At::L330 => {
                delta = alphak + dmin1(0.0, v.dst0);
                twopsi = alphak * dst * dst + gtsta;
                if *ka >= kamin || psifac >= big {
                    At::L340
                } else if delta >= psifac * twopsi {
                    At::L370
                } else {
                    At::L340
                }
            }
            At::L340 => {
                {
                    let (head, tail) = w.split_at_mut(x - 1);
                    t = dl7svn(p, l, &mut tail[..p], &mut head[..p]);
                }
                for i in 0..p {
                    w[i] = t * w[i];
                }
                dl7itv(p, w, l);
                t2 = 1.0 / dv2nrm(&w[..p]);
                for i in 0..p {
                    w[i] = t2 * w[i];
                }
                t = t2 * t;
                sw = dd7tpr(&w[q - 1..q - 1 + p], &w[..p]);
                t1 = (rad + dst) * (rad - dst);
                let mut root = (sw * sw + t1).sqrt();
                if sw < 0.0 {
                    root = -root;
                }
                si = t1 / (sw + root);
                if square(t2 * si) <= eps * (square(dst) + alphak * radsq) {
                    At::L380
                } else {
                    if v.dst0 <= 0.0 {
                        v.dst0 = dmin1(v.dst0, square(t2) - alphak);
                    }
                    lk = dmax1(lk, -v.dst0);
                    At::L370
                }
            }
            At::L370 => {
                if delta > dgxfac * w[dggdmx - 1] {
                    At::L250
                } else {
                    At::L270
                }
            }
            At::L380 => {
                alphak = -alphak;
                v.preduc = 0.5 * twopsi;
                t1 = 0.0;
                let product = dd7tpr(&w[x - 1..x - 1 + p], &w[..p]);
                t = si * (alphak * sw - 0.5 * si * (alphak + t * product));
                if !(t < eps * twopsi / 6.0) {
                    v.preduc = v.preduc + t;
                    dst = rad;
                    t1 = -si;
                }
                for i in 1..=p {
                    let j = q0 + i;
                    w[j - 1] = t1 * w[i - 1] - w[j - 1];
                    step[i - 1] = w[j - 1] / d[i - 1];
                }
                v.gtstep = dd7tpr(&dig[..p], &w[q - 1..q - 1 + p]);
                At::L410
            }
            At::L410 => {
                v.dstnrm = dst;
                v.stppar = alphak;
                w[lk0 - 1] = lk;
                w[uk0 - 1] = uk;
                v.rad0 = rad;
                w[dstsav - 1] = dst;
                let mut j = 0;
                for i in 1..=p {
                    j += i;
                    dihdi[j - 1] = w[diag0 + i - 1];
                }
                return;
            }
        }
    }
}

/// `DG7QSB`: the More-Hebden step under bounds, which alternates `DG7QTS` in the free
/// variables with `DS7BQN`'s search along the step until the step reaches no new bound.
#[allow(clippy::too_many_arguments)]
fn dg7qsb(
    lower: &[f64],
    upper: &[f64],
    d: &[f64],
    dihdi: &mut [f64],
    g: &[f64],
    ipiv: &mut [i32],
    ipiv1: &mut [i32],
    ipiv2: &mut [i32],
    ka: &mut i32,
    l: &mut [f64],
    p: usize,
    p0: &mut i32,
    pc: usize,
    step: &mut [f64],
    td: &mut [f64],
    tg: &mut [f64],
    v: &mut Scalars,
    w: &mut [f64],
    x: &mut [f64],
    x0: &[f64],
) {
    let mut p1 = pc;
    let mut nred = 0.0;
    let mut ds0 = 0.0;
    if *ka >= 0 {
        nred = v.nreduc;
        ds0 = v.dst0;
    } else {
        *p0 = 0;
        *ka = -1;
    }
    let mut kinit = -1;
    if *p0 == p1 as i32 {
        kinit = *ka;
    }
    x[..p].copy_from_slice(&x0[..p]);
    let mut pred = 0.0;
    let rad = v.radius;
    let mut kb = -1;
    v.dstnrm = 0.0;
    let (step, dst) = step.split_at_mut(p);
    if p1 == 0 {
        nred = 0.0;
        ds0 = 0.0;
        step.fill(0.0);
    } else {
        td[..p].copy_from_slice(&d[..p]);
        dv7ipr(ipiv, &mut td[..p]);
        dv7vmp_divide(&mut tg[..p], &g[..p], &d[..p]);
        dv7ipr(ipiv, &mut tg[..p]);
        loop {
            let mut k = kinit;
            kinit = -1;
            v.radius = rad - v.dstnrm;
            dg7qts(td, tg, dihdi, &mut k, l, p1, step, v, w);
            *p0 = p1 as i32;
            if *ka < 0 {
                nred = v.nreduc;
                ds0 = v.dst0;
            }
            *ka = k;
            v.radius = rad;
            let p10 = p1;
            let ns = ds7bqn(
                lower, upper, d, dst, ipiv, ipiv1, ipiv2, &mut kb, l, p, &mut p1, step, td, tg,
                v, w, x, x0,
            );
            if ns > 0 {
                ds7ipr(p10, ipiv1, dihdi);
            }
            pred += v.preduc;
            if ns != 0 {
                *p0 = 0;
            }
            if kb > 0 {
                break;
            }
        }
    }
    v.dst0 = ds0;
    v.nreduc = nred;
    v.preduc = pred;
    v.gtstep = dd7tpr(&g[..p], step);
}

/// `DS7BQN`: follow the step from `DG7QTS` to the first bound it crosses, fix that
/// variable, and continue along the projected step. Returns how many variables it moved.
#[allow(clippy::too_many_arguments)]
fn ds7bqn(
    lower: &[f64],
    upper: &[f64],
    d: &[f64],
    dst: &mut [f64],
    ipiv: &mut [i32],
    ipiv1: &mut [i32],
    ipiv2: &mut [i32],
    kb: &mut i32,
    l: &mut [f64],
    p: usize,
    p1: &mut usize,
    step: &mut [f64],
    td: &mut [f64],
    tg: &mut [f64],
    v: &mut Scalars,
    w: &mut [f64],
    x: &mut [f64],
    x0: &[f64],
) -> usize {
    const FUDGE: f64 = 1.0001;
    let meps2 = 2.0 * f64::EPSILON;
    let dstmax = FUDGE * (1.0 + v.phmxfc) * v.radius;
    let dstmin = (1.0 + v.phmnfc) * v.radius;
    let mut dst1 = 0.0;
    let p0 = *p1;
    let mut ns = 0;
    for i in 0..p {
        ipiv1[i] = i as i32 + 1;
        ipiv2[i] = i as i32 + 1;
    }
    for i in 0..*p1 {
        w[i] = -step[i] * td[i];
    }
    let alpha = v.stppar.abs();
    v.preduc = 0.0;
    let mut gts = -v.gtstep;
    if *kb < 0 {
        dst[..p].fill(0.0);
    }
    *kb = 1;
    loop {
        // Statement 30.
        let mut t = 1.0;
        let mut k: i64 = 0;
        for i in 1..=*p1 {
            let j = ipiv[i - 1] as usize;
            let dx = w[i - 1] / d[j - 1];
            let xi = x[j - 1] - dx;
            let ti = if xi < lower[j - 1] {
                k = -(i as i64);
                (x[j - 1] - lower[j - 1]) / dx
            } else if xi <= upper[j - 1] {
                continue;
            } else {
                k = i as i64;
                (x[j - 1] - upper[j - 1]) / dx
            };
            if !(t <= ti) {
                t = ti;
            }
        }
        if p > *p1 {
            step[*p1..p].copy_from_slice(&dst[*p1..p]);
        }
        for i in 0..*p1 {
            step[i] = -t * w[i] + dst[i];
        }
        let dst0 = dst1;
        dst1 = dv2nrm(&step[..p]);
        if dst1 > dstmax {
            if *p1 < p0 {
                if dst0 < dstmin {
                    *kb = 0;
                }
                break;
            }
            k = 0;
        }
        // Statement 80.
        v.dstnrm = dst1;
        dst[..*p1].copy_from_slice(&step[..*p1]);
        let t1 = 1.0 - t;
        for i in 0..*p1 {
            tg[i] = t1 * tg[i];
        }
        if alpha > 0.0 {
            for i in 0..*p1 {
                tg[i] = t * alpha * w[i] + tg[i];
            }
        }
        v.preduc = v.preduc
            + t * ((1.0 - 0.5 * t) * gts + 0.5 * alpha * t * dd7tpr(&w[..*p1], &w[..*p1]));
        if k == 0 {
            break;
        }
        let p1m1 = *p1 - 1;
        let j = k.unsigned_abs() as usize;
        if j != *p1 {
            ns += 1;
            ipiv2[*p1 - 1] = j as i32;
            dq7rsh(j, *p1, l, w);
            i7shft(*p1, j, ipiv);
            i7shft(*p1, j, ipiv1);
            dv7shf(*p1, j, tg);
            dv7shf(*p1, j, dst);
        }
        if k < 0 {
            ipiv[*p1 - 1] = -ipiv[*p1 - 1];
        }
        *p1 = p1m1;
        if *p1 == 0 {
            break;
        }
        w[..*p1].copy_from_slice(&tg[..*p1]);
        dl7ivm(*p1, w, l);
        gts = dd7tpr(&w[..*p1], &w[..*p1]);
        dl7itv(*p1, w, l);
    }
    // Statement 110.
    for i in 0..p {
        let j = ipiv[i].unsigned_abs() as usize;
        step[j - 1] = dst[i] / d[j - 1];
    }
    if *p1 < p0 {
        for i in *p1 + 1..=p0 {
            let mut j = ipiv[i - 1];
            let mut t = meps2;
            if j <= 0 {
                t = -t;
                j = -j;
                ipiv[i - 1] = j;
            }
            let j = j as usize;
            t = t * dmax1(x[j - 1].abs(), x0[j - 1].abs());
            step[j - 1] = step[j - 1] + t;
        }
    }
    for i in 0..p {
        x[i] = 1.0 * step[i] + x0[i];
    }
    if ns > 0 {
        dv7ipr(&ipiv1[..p0], &mut td[..p0]);
    }
    ns
}

/// `DS7GRD`: Stewart's finite-difference gradient, one component per call. `irc` is 0 on
/// the first call; a nonzero return asks for the objective at `x`, and 0 means `g` is done.
#[allow(clippy::too_many_arguments)]
fn ds7grd(
    alpha: &[f64],
    d: &[f64],
    eta0: f64,
    fx: &mut f64,
    g: &mut [f64],
    irc: &mut i32,
    n: usize,
    w: &mut [f64; 6],
    x: &mut [f64],
) {
    const FH: usize = 2;
    const FX0: usize = 3;
    const HSAVE: usize = 4;
    const XISAVE: usize = 5;
    let mut i;
    let h;
    if *irc < 0 {
        // Statement 140: the second half of a central difference.
        let hh = -w[HSAVE];
        i = irc.unsigned_abs() as usize;
        if !(hh > 0.0) {
            w[FH] = *fx;
            x[i - 1] = w[XISAVE] + hh;
            w[HSAVE] = hh;
            return;
        }
        g[i - 1] = (w[FH] - *fx) / (2.0 * hh);
        x[i - 1] = w[XISAVE];
    } else if *irc == 0 {
        w[0] = f64::EPSILON;
        w[1] = w[0].sqrt();
        w[FX0] = *fx;
    } else {
        let i = *irc as usize;
        g[i - 1] = (*fx - w[FX0]) / w[HSAVE];
        x[i - 1] = w[XISAVE];
    }
    // Statement 110.
    i = irc.unsigned_abs() as usize + 1;
    if i > n {
        *fx = w[FX0];
        *irc = 0;
        return;
    }
    *irc = i as i32;
    let afx = w[FX0].abs();
    let machep = w[0];
    let h0 = w[1];
    let hmin = 50.0 * machep;
    w[XISAVE] = x[i - 1];
    let axi = x[i - 1].abs();
    let axibar = dmax1(axi, 1.0 / d[i - 1]);
    let gi = g[i - 1];
    let agi = gi.abs();
    let mut eta = eta0.abs();
    if afx > 0.0 {
        eta = dmax1(eta, agi * axi * machep / afx);
    }
    let alphai = alpha[i - 1];
    if alphai == 0.0 {
        h = axibar;
    } else if gi == 0.0 || *fx == 0.0 {
        h = h0 * axibar;
    } else {
        let afxeta = afx * eta;
        let aai = alphai.abs();
        let mut hh;
        if square(gi) <= afxeta * aai {
            hh = 2.0 * (afxeta * agi).powf(1.0 / 3.0) * aai.powf(-2.0 / 3.0);
            hh = hh * (1.0 - 2.0 * agi / (3.0 * aai * hh + 4.0 * agi));
        } else {
            hh = 2.0 * (afxeta / aai).sqrt();
            hh = hh * (1.0 - aai * hh / (3.0 * aai * hh + 4.0 * agi));
        }
        hh = dmax1(hh, hmin * axibar);
        if aai * hh <= 0.002 * agi {
            if hh >= 0.02 * axibar {
                hh = h0 * axibar;
            }
            if alphai * gi < 0.0 {
                hh = -hh;
            }
        } else {
            let discon = 2000.0 * afxeta;
            hh = discon / (agi + (square(gi) + aai * discon).sqrt());
            hh = dmax1(hh, hmin * axibar);
            if hh >= 0.02 * axibar {
                hh = axibar * h0.powf(2.0 / 3.0);
            }
            *irc = -(i as i32);
        }
        h = hh;
    }
    // Statement 200.
    x[i - 1] = w[XISAVE] + h;
    w[HSAVE] = h;
}

/// `DS3GRD`: `DS7GRD` under bounds, which keeps every difference inside the box and
/// switches to a one-sided three-point formula next to a bound.
#[allow(clippy::too_many_arguments)]
fn ds3grd(
    alpha: &[f64],
    lower: &[f64],
    upper: &[f64],
    d: &[f64],
    eta0: f64,
    fx: &mut f64,
    g: &mut [f64],
    irc: &mut i32,
    p: usize,
    w: &mut [f64; 6],
    x: &mut [f64],
) {
    const FH: usize = 2;
    const FX0: usize = 3;
    const HSAVE: usize = 4;
    const XISAVE: usize = 5;
    #[derive(Clone, Copy)]
    enum At {
        L20,
        L150,
        L190,
        L200,
    }
    let mut i: usize = 0;
    let mut h = 0.0;
    let mut xi = 0.0;
    let mut xih = 0.0;
    let p_signed = p as i32;
    let mut at = if *irc < 0 {
        // Statement 80.
        let ii = -*irc;
        if ii <= p_signed {
            // Statement 100: the second half of a central difference.
            i = ii as usize;
            h = -w[HSAVE];
            if !(h > 0.0) {
                w[FH] = *fx;
                xih = w[XISAVE] + h;
                At::L200
            } else {
                g[i - 1] = (w[FH] - *fx) / (2.0 * h);
                x[i - 1] = w[XISAVE];
                At::L20
            }
        } else if ii - p_signed <= p_signed {
            i = (ii - p_signed) as usize;
            w[FH] = *fx;
            h = 2.0 * w[HSAVE];
            xih = w[XISAVE] + h;
            *irc -= p_signed;
            At::L200
        } else {
            i = (ii - 2 * p_signed) as usize;
            g[i - 1] = (4.0 * w[FH] - *fx - 3.0 * w[FX0]) / w[HSAVE];
            *irc = i as i32;
            x[i - 1] = w[XISAVE];
            At::L20
        }
    } else if *irc == 0 {
        w[0] = f64::EPSILON;
        w[1] = w[0].sqrt();
        w[FX0] = *fx;
        At::L20
    } else {
        let i = *irc as usize;
        g[i - 1] = (*fx - w[FX0]) / w[HSAVE];
        x[i - 1] = w[XISAVE];
        At::L20
    };
    loop {
        at = match at {
            At::L20 => {
                i = irc.unsigned_abs() as usize + 1;
                if i > p {
                    *irc = 0;
                    *fx = w[FX0];
                    return;
                }
                *irc = i as i32;
                if !(lower[i - 1] < upper[i - 1]) {
                    g[i - 1] = 0.0;
                    continue;
                }
                let afx = w[FX0].abs();
                let machep = w[0];
                let h0 = w[1];
                let hmin = 50.0 * machep;
                xi = x[i - 1];
                w[XISAVE] = xi;
                let axi = xi.abs();
                let axibar = dmax1(axi, 1.0 / d[i - 1]);
                let gi = g[i - 1];
                let agi = gi.abs();
                let mut eta = eta0.abs();
                if afx > 0.0 {
                    eta = dmax1(eta, agi * axi * machep / afx);
                }
                let alphai = alpha[i - 1];
                if alphai == 0.0 {
                    h = axibar;
                    At::L150
                } else if gi == 0.0 || *fx == 0.0 {
                    h = h0 * axibar;
                    At::L150
                } else {
                    let afxeta = afx * eta;
                    let aai = alphai.abs();
                    if square(gi) <= afxeta * aai {
                        h = 2.0 * (afxeta * agi).powf(1.0 / 3.0) * aai.powf(-2.0 / 3.0);
                        h = h * (1.0 - 2.0 * agi / (3.0 * aai * h + 4.0 * agi));
                    } else {
                        h = 2.0 * (afxeta / aai).sqrt();
                        h = h * (1.0 - aai * h / (3.0 * aai * h + 4.0 * agi));
                    }
                    h = dmax1(h, hmin * axibar);
                    if aai * h <= 0.002 * agi {
                        if h >= 0.02 * axibar {
                            h = h0 * axibar;
                        }
                        if alphai * gi < 0.0 {
                            h = -h;
                        }
                        At::L150
                    } else {
                        let discon = 2000.0 * afxeta;
                        h = discon / (agi + (square(gi) + aai * discon).sqrt());
                        h = dmax1(h, hmin * axibar);
                        if h >= 0.02 * axibar {
                            h = axibar * h0.powf(2.0 / 3.0);
                        }
                        xih = xi + h;
                        if xi - h < lower[i - 1] {
                            if xi + 2.0 * h > upper[i - 1] {
                                At::L190
                            } else {
                                *irc = -((i + p) as i32);
                                At::L200
                            }
                        } else {
                            *irc = -(i as i32);
                            if xih <= upper[i - 1] {
                                At::L200
                            } else {
                                h = -h;
                                xih = xi + h;
                                if xi + 2.0 * h < lower[i - 1] {
                                    At::L190
                                } else {
                                    *irc = -((i + p) as i32);
                                    At::L200
                                }
                            }
                        }
                    }
                }
            }
            At::L150 => {
                let mut hit = false;
                loop {
                    xih = xi + h;
                    let inside = if h > 0.0 {
                        xih <= upper[i - 1]
                    } else {
                        xih >= lower[i - 1]
                    };
                    if inside {
                        break At::L200;
                    }
                    if hit {
                        break At::L190;
                    }
                    hit = true;
                    h = -h;
                }
            }
            At::L190 => {
                *irc = (i + p) as i32;
                *fx = w[FX0];
                return;
            }
            At::L200 => {
                x[i - 1] = xih;
                w[HSAVE] = h;
                return;
            }
        }
    }
}

/// `DW7ZBF`: the vectors `w` and `z` of the BFGS update `L+ = L (I + z w')` for the step
/// `s` and gradient change `y`, damped (Powell's modification) when `y's` is small.
fn dw7zbf(l: &[f64], n: usize, s: &[f64], w: &mut [f64], y: &[f64], z: &mut [f64]) {
    const EPS: f64 = 0.1;
    w[..n].copy_from_slice(&s[..n]);
    dl7tvm(n, w, l);
    let shs = dd7tpr(&w[..n], &w[..n]);
    let ys = dd7tpr(&y[..n], &s[..n]);
    let (cy, cs);
    if ys >= EPS * shs {
        cy = 1.0 / (ys.sqrt() * shs.sqrt());
        cs = 1.0 / shs;
    } else {
        let theta = (1.0 - EPS) * shs / (shs - ys);
        let epsrt = EPS.sqrt();
        cy = theta / (shs * epsrt);
        cs = (1.0 + (theta - 1.0) / epsrt) / shs;
    }
    z[..n].copy_from_slice(&y[..n]);
    dl7ivm(n, z, l);
    for i in 0..n {
        z[i] = cy * z[i] - cs * w[i];
    }
}

/// `DL7UPD`: the Cholesky factor of `L (I + z w') (I + w z') L'`, computed in place in `l`;
/// `beta`, `gamma` and `lambda` are scratch, and `w` and `z` are overwritten.
fn dl7upd(
    beta: &mut [f64],
    gamma: &mut [f64],
    l: &mut [f64],
    lambda: &mut [f64],
    n: usize,
    w: &mut [f64],
    z: &mut [f64],
) {
    let mut nu = 1.0;
    let mut eta = 0.0;
    if n > 1 {
        let nm1 = n - 1;
        let mut s = 0.0;
        for i in 1..=nm1 {
            let j = n - i;
            s = s + square(w[j]);
            lambda[j - 1] = s;
        }
        for j in 1..=nm1 {
            let wj = w[j - 1];
            let a = nu * z[j - 1] - eta * wj;
            let theta = 1.0 + a * wj;
            let s = a * lambda[j - 1];
            let mut lj = (square(theta) + a * s).sqrt();
            if theta > 0.0 {
                lj = -lj;
            }
            lambda[j - 1] = lj;
            let b = theta * wj + s;
            gamma[j - 1] = b * nu / lj;
            beta[j - 1] = (a - b * eta) / lj;
            nu = -nu / lj;
            eta = -(eta + square(a) / (theta - lj)) / lj;
        }
    }
    lambda[n - 1] = 1.0 + (nu * z[n - 1] - eta * w[n - 1]) * w[n - 1];
    let np1 = n + 1;
    let mut jj = n * (n + 1) / 2;
    for k in 1..=n {
        let j = np1 - k;
        let lj = lambda[j - 1];
        let ljj = l[jj - 1];
        l[jj - 1] = lj * ljj;
        let wj = w[j - 1];
        w[j - 1] = ljj * wj;
        let zj = z[j - 1];
        z[j - 1] = ljj * zj;
        if k != 1 {
            let bj = beta[j - 1];
            let gj = gamma[j - 1];
            let mut ij = jj + j;
            for i in j + 1..=n {
                let lij = l[ij - 1];
                l[ij - 1] = lj * lij + bj * w[i - 1] + gj * z[i - 1];
                w[i - 1] = w[i - 1] + lij * wj;
                z[i - 1] = z[i - 1] + lij * zj;
                ij += i;
            }
        }
        jj -= j;
    }
}

/// `DQ7RSH`: move column `k` of the upper triangle `R` (here `L'`, so `l` packed by rows)
/// to position `p`, shifting columns `k+1..p` left, and restore triangularity with
/// Householder-style plane rotations; `w` is scratch.
fn dq7rsh(k: usize, p: usize, r: &mut [f64], w: &mut [f64]) {
    if k >= p {
        return;
    }
    let km1 = k - 1;
    let mut k1 = k * km1 / 2;
    w[..k].copy_from_slice(&r[k1..k1 + k]);
    let mut wj = w[k - 1];
    let pm1 = p - 1;
    let mut j1 = k1 + km1;
    for j in k..=pm1 {
        let jm1 = j - 1;
        let jp1 = j + 1;
        if jm1 > 0 {
            r.copy_within(j1 + 1..j1 + 1 + jm1, k1);
        }
        j1 += jp1;
        k1 += j;
        let a = r[j1 - 1];
        let b = r[j1];
        let (x, z);
        if b == 0.0 {
            r[k1 - 1] = a;
            x = 0.0;
            z = 0.0;
        } else {
            let (value, rotation_x, rotation_y, rotation_z) = dh2rfg(a, b);
            r[k1 - 1] = value;
            x = rotation_x;
            z = rotation_z;
            if j != pm1 {
                let mut i1 = j1;
                for i in jp1..=pm1 {
                    i1 += i;
                    let t = r[i1 - 1] * rotation_x + r[i1] * rotation_y;
                    r[i1 - 1] = r[i1 - 1] + t;
                    r[i1] = r[i1] + t * rotation_z;
                }
            }
        }
        let t = x * wj;
        w[j - 1] = wj + t;
        wj = t * z;
    }
    w[p - 1] = wj;
    r[k1..k1 + p].copy_from_slice(&w[..p]);
}

/// `DH2RFG`: the 2x2 Householder reflection that zeroes `b`; returns the new `a` and the
/// reflection's `x`, `y` and `z`.
fn dh2rfg(a: f64, b: f64) -> (f64, f64, f64, f64) {
    if b == 0.0 {
        return (a, 0.0, 0.0, 0.0);
    }
    let t = a.abs() + b.abs();
    let mut a1 = a / t;
    let b1 = b / t;
    let mut c = (square(a1) + square(b1)).sqrt();
    if a1 > 0.0 {
        c = -c;
    }
    a1 = a1 - c;
    let z = b1 / a1;
    let x = a1 / c;
    let y = b1 / c;
    (t * c, x, y, z)
}

/// `DL7SRT`: the Cholesky factor `l` of the packed symmetric `a`. Returns 0 on success, or
/// the 1-based row where `a` stopped being positive definite (whose diagonal then holds the
/// nonpositive pivot).
fn dl7srt(n: usize, l: &mut [f64], a: &[f64]) -> usize {
    let mut i0 = 0;
    for i in 1..=n {
        let mut td = 0.0;
        let mut j0 = 0;
        for j in 1..i {
            let mut t = 0.0;
            for k in 1..j {
                t = t + l[i0 + k - 1] * l[j0 + k - 1];
            }
            let ij = i0 + j;
            j0 += j;
            t = (a[ij - 1] - t) / l[j0 - 1];
            l[ij - 1] = t;
            td = td + t * t;
        }
        i0 += i;
        let t = a[i0 - 1] - td;
        if t <= 0.0 {
            l[i0 - 1] = t;
            return i;
        }
        l[i0 - 1] = t.sqrt();
    }
    0
}

/// `DL7SVN`: an estimate of the smallest singular value of `L`, with `x` and `y` scratch;
/// `y` ends as the approximate left singular vector.
fn dl7svn(p: usize, l: &[f64], x: &mut [f64], y: &mut [f64]) -> f64 {
    const R9973: f64 = 9973.0;
    let mut ix: i64 = 2;
    let pm1 = p - 1;
    let mut ii = 0;
    let mut j0 = p * pm1 / 2;
    let mut jj = j0 + p;
    if l[jj - 1] == 0.0 {
        return 0.0;
    }
    ix = (3432 * ix) % 9973;
    let mut b = 0.5 * (1.0 + ix as f64 / R9973);
    let mut xplus = b / l[jj - 1];
    x[p - 1] = xplus;
    if p > 1 {
        for i in 1..=pm1 {
            ii += i;
            if l[ii - 1] == 0.0 {
                return 0.0;
            }
            let ji = j0 + i;
            x[i - 1] = xplus * l[ji - 1];
        }
        for jjj in 1..=pm1 {
            let j = p - jjj;
            ix = (3432 * ix) % 9973;
            b = 0.5 * (1.0 + ix as f64 / R9973);
            xplus = b - x[j - 1];
            let mut xminus = -b - x[j - 1];
            let mut splus = xplus.abs();
            let mut sminus = xminus.abs();
            let jm1 = j - 1;
            j0 = j * jm1 / 2;
            jj = j0 + j;
            xplus = xplus / l[jj - 1];
            xminus = xminus / l[jj - 1];
            for i in 1..=jm1 {
                let ji = j0 + i;
                splus = splus + (x[i - 1] + l[ji - 1] * xplus).abs();
                sminus = sminus + (x[i - 1] + l[ji - 1] * xminus).abs();
            }
            if sminus > splus {
                xplus = xminus;
            }
            x[j - 1] = xplus;
            for i in 0..jm1 {
                x[i] = xplus * l[j0 + i] + x[i];
            }
        }
    }
    let t = 1.0 / dv2nrm(&x[..p]);
    for entry in &mut x[..p] {
        *entry = t * *entry;
    }
    for j in 1..=p {
        let jm1 = j - 1;
        let j0 = j * jm1 / 2;
        let jj = j0 + j;
        let mut t = 0.0;
        if jm1 > 0 {
            t = dd7tpr(&l[j0..j0 + jm1], &y[..jm1]);
        }
        y[j - 1] = (x[j - 1] - t) / l[jj - 1];
    }
    1.0 / dv2nrm(&y[..p])
}

/// `DS7LVM`: `y = S x` for the packed symmetric `S`.
fn ds7lvm(p: usize, y: &mut [f64], s: &[f64], x: &[f64]) {
    let mut j = 1;
    for i in 1..=p {
        y[i - 1] = dd7tpr(&s[j - 1..j - 1 + i], &x[..i]);
        j += i;
    }
    if p <= 1 {
        return;
    }
    j = 1;
    for i in 2..=p {
        let xi = x[i - 1];
        j += 1;
        for k in 1..i {
            y[k - 1] = y[k - 1] + s[j - 1] * xi;
            j += 1;
        }
    }
}

/// `DS7IPR`: permute the rows and columns of the packed symmetric `h`, so that the new
/// `h(i, j)` is the old `h(ip(i), ip(j))`. PORT swaps in place; this gathers from a copy,
/// which moves the same values.
fn ds7ipr(p: usize, ip: &[i32], h: &mut [f64]) {
    let old = h[..p * (p + 1) / 2].to_vec();
    let packed = |row: usize, column: usize| {
        let (row, column) = if row >= column {
            (row, column)
        } else {
            (column, row)
        };
        row * (row + 1) / 2 + column
    };
    for i in 0..p {
        for j in 0..=i {
            let source = packed(ip[i] as usize - 1, ip[j] as usize - 1);
            h[i * (i + 1) / 2 + j] = old[source];
        }
    }
}

/// `DL7IVM`: `x = L^-1 x`, with `x` holding the right-hand side on entry. Leading zeros of
/// the right-hand side become positive zeros, as when PORT writes into a separate vector.
fn dl7ivm(n: usize, x: &mut [f64], l: &[f64]) {
    let Some(k) = x[..n].iter().position(|value| *value != 0.0) else {
        x[..n].fill(0.0);
        return;
    };
    x[..k].fill(0.0);
    let mut j = (k + 1) * (k + 2) / 2;
    x[k] = x[k] / l[j - 1];
    for i in k + 2..=n {
        let t = dd7tpr(&l[j..j + i - 1], &x[..i - 1]);
        j += i;
        x[i - 1] = (x[i - 1] - t) / l[j - 1];
    }
}

/// `DL7ITV`: `x = L'^-1 x`, in place.
fn dl7itv(n: usize, x: &mut [f64], l: &[f64]) {
    let mut i0 = n * (n + 1) / 2;
    for ii in 1..=n {
        let i = n + 1 - ii;
        let xi = x[i - 1] / l[i0 - 1];
        x[i - 1] = xi;
        if i <= 1 {
            return;
        }
        i0 -= i;
        if xi == 0.0 {
            continue;
        }
        for j in 1..i {
            x[j - 1] = x[j - 1] - xi * l[i0 + j - 1];
        }
    }
}

/// `DL7TVM`: `x = L' x`, in place.
fn dl7tvm(n: usize, x: &mut [f64], l: &[f64]) {
    let mut i0 = 0;
    for i in 1..=n {
        let yi = x[i - 1];
        x[i - 1] = 0.0;
        for j in 1..=i {
            x[j - 1] = x[j - 1] + yi * l[i0 + j - 1];
        }
        i0 += i;
    }
}

/// `DL7VML`: `x = L x`, in place.
fn dl7vml(n: usize, x: &mut [f64], l: &[f64]) {
    let mut i0 = n * (n + 1) / 2;
    for ii in 1..=n {
        let i = n + 1 - ii;
        i0 -= i;
        let mut t = 0.0;
        for j in 1..=i {
            t = t + l[i0 + j - 1] * x[j - 1];
        }
        x[i - 1] = t;
    }
}

/// `DD7TPR`: the inner product, skipping terms too small to matter so that it cannot
/// underflow.
fn dd7tpr(x: &[f64], y: &[f64]) -> f64 {
    let sqteta = sqteta();
    let mut sum = 0.0;
    for (x, y) in x.iter().zip(y) {
        let t = dmax1(x.abs(), y.abs());
        if !(t > 1.0) {
            if t < sqteta {
                continue;
            }
            let t = (x / sqteta) * y;
            if t.abs() < sqteta {
                continue;
            }
        }
        sum = sum + x * y;
    }
    sum
}

/// `DV2NRM`, the 2-norm. GNU R hands PORT's norm to the BLAS `dnrm2`, which its iterates
/// show to the last bit, so this is reference LAPACK 3.12's `dnrm2`: Blue's algorithm, which
/// sums the squares of mid-range values directly and scales only values near overflow or
/// underflow.
fn dv2nrm(x: &[f64]) -> f64 {
    let tsml = 2f64.powi(-511);
    let tbig = 2f64.powi(486);
    let ssml = 2f64.powi(537);
    let sbig = 2f64.powi(-538);
    let mut notbig = true;
    let (mut asml, mut amed, mut abig) = (0.0, 0.0, 0.0);
    for value in x {
        let ax = value.abs();
        if ax > tbig {
            abig = abig + square(ax * sbig);
            notbig = false;
        } else if ax < tsml {
            if notbig {
                asml = asml + square(ax * ssml);
            }
        } else {
            amed = amed + ax * ax;
        }
    }
    let (scale, sumsq) = if abig > 0.0 {
        if amed > 0.0 || amed.is_nan() {
            abig = abig + (amed * sbig) * sbig;
        }
        (1.0 / sbig, abig)
    } else if asml > 0.0 {
        if amed > 0.0 || amed.is_nan() {
            let amed = amed.sqrt();
            let asml = asml.sqrt() / ssml;
            let (ymin, ymax) = if asml > amed { (amed, asml) } else { (asml, amed) };
            (1.0, ymax * ymax * (1.0 + square(ymin / ymax)))
        } else {
            (1.0 / ssml, asml)
        }
    } else {
        (1.0, amed)
    };
    scale * sumsq.sqrt()
}

/// `DRLDST`: the relative distance between `x` and `x0` in the scaled norm.
fn drldst(d: &[f64], x: &[f64], x0: &[f64]) -> f64 {
    let mut emax = 0.0;
    let mut xmax = 0.0;
    for i in 0..d.len() {
        let t = (d[i] * (x[i] - x0[i])).abs();
        if emax < t {
            emax = t;
        }
        let t = d[i] * (x[i].abs() + x0[i].abs());
        if xmax < t {
            xmax = t;
        }
    }
    if xmax > 0.0 { emax / xmax } else { 0.0 }
}

/// `DV2AXY`: `w = a x + y`.
fn dv2axy(w: &mut [f64], a: f64, x: &[f64], y: &[f64]) {
    for i in 0..w.len() {
        w[i] = a * x[i] + y[i];
    }
}

/// `DV7VMP` with `K < 0`: `x = y / z`.
fn dv7vmp_divide(x: &mut [f64], y: &[f64], z: &[f64]) {
    for i in 0..x.len() {
        x[i] = y[i] / z[i];
    }
}

/// `DV7VMP` with `K >= 0`: `x = y * z`.
fn dv7vmp_multiply(x: &mut [f64], y: &[f64], z: &[f64]) {
    for i in 0..x.len() {
        x[i] = y[i] * z[i];
    }
}

/// `DV7IPR`: permute `x` so that the new `x(i)` is the old `x(ip(i))`.
fn dv7ipr(ip: &[i32], x: &mut [f64]) {
    let old = x.to_vec();
    for (entry, position) in x.iter_mut().zip(ip) {
        *entry = old[*position as usize - 1];
    }
}

/// `I7SHFT` for `k > 0`: move entry `k` to position `n`, shifting the ones after it left.
fn i7shft(n: usize, k: usize, x: &mut [i32]) {
    if k >= n {
        return;
    }
    x[k - 1..n].rotate_left(1);
}

/// `DV7SHF`: `I7SHFT` for a real vector.
fn dv7shf(n: usize, k: usize, x: &mut [f64]) {
    if k >= n {
        return;
    }
    x[k - 1..n].rotate_left(1);
}

/// `DR7MDC(2)`: the square root of 256 times the smallest normal number, over 16.
fn sqteta() -> f64 {
    (256.0 * f64::MIN_POSITIVE).sqrt() / 16.0
}

/// Fortran's `DMAX1` for two arguments.
fn dmax1(a: f64, b: f64) -> f64 {
    if b > a { b } else { a }
}

/// Fortran's `DMIN1` for two arguments.
fn dmin1(a: f64, b: f64) -> f64 {
    if b < a { b } else { a }
}

/// Fortran's `X**2`, which compiles to one multiplication.
fn square(x: f64) -> f64 {
    x * x
}
