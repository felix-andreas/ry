use std::fmt::Display;
use std::hint::black_box;
use std::time::Instant;

// The shocks must be the exact numbers GNU R draws after set.seed(9), so this ports R's default
// generator: Mersenne Twister seeded the way R scrambles the seed, and normals by inversion.
struct RGenerator {
    state: [u32; 624],
    index: usize,
}

impl RGenerator {
    fn new(seed: u32) -> RGenerator {
        let mut scrambled = seed;
        for _ in 0..50 {
            scrambled = scrambled.wrapping_mul(69069).wrapping_add(1);
        }
        // R fills 625 seed words and then overwrites the first, the position counter, with 624.
        scrambled = scrambled.wrapping_mul(69069).wrapping_add(1);
        let mut state = [0u32; 624];
        for word in state.iter_mut() {
            scrambled = scrambled.wrapping_mul(69069).wrapping_add(1);
            *word = scrambled;
        }
        RGenerator { state, index: 624 }
    }

    fn next_word(&mut self) -> u32 {
        const UPPER: u32 = 0x8000_0000;
        const LOWER: u32 = 0x7fff_ffff;
        const MATRIX: u32 = 0x9908_b0df;
        if self.index >= 624 {
            for k in 0..624 {
                let y = (self.state[k] & UPPER) | (self.state[(k + 1) % 624] & LOWER);
                let twist = if y & 1 == 1 { MATRIX } else { 0 };
                self.state[k] = self.state[(k + 397) % 624] ^ (y >> 1) ^ twist;
            }
            self.index = 0;
        }
        let mut y = self.state[self.index];
        self.index += 1;
        y ^= y >> 11;
        y ^= (y << 7) & 0x9d2c_5680;
        y ^= (y << 15) & 0xefc6_0000;
        y ^ (y >> 18)
    }

    fn uniform(&mut self) -> f64 {
        const I2_32M1: f64 = 2.328306437080797e-10;
        let value = self.next_word() as f64 * 2.3283064365386963e-10;
        if value <= 0.0 {
            0.5 * I2_32M1
        } else if 1.0 - value <= 0.0 {
            1.0 - 0.5 * I2_32M1
        } else {
            value
        }
    }

    fn normal(&mut self) -> f64 {
        const BIG: f64 = 134_217_728.0;
        let high = (BIG * self.uniform()).trunc();
        quantile_normal((high + self.uniform()) / BIG)
    }
}

// Wichura's AS 241, as in R's qnorm for the standard normal.
fn quantile_normal(p: f64) -> f64 {
    let q = p - 0.5;
    if q.abs() <= 0.425 {
        let r = 0.180625 - q * q;
        return q
            * (((((((r * 2509.0809287301226727 + 33430.575583588128105) * r + 67265.770927008700853) * r
                + 45921.953931549871457)
                * r
                + 13731.693765509461125)
                * r
                + 1971.5909503065514427)
                * r
                + 133.14166789178437745)
                * r
                + 3.387132872796366608)
            / (((((((r * 5226.495278852545925 + 28729.085735721942674) * r + 39307.89580009271061) * r
                + 21213.794301586595867)
                * r
                + 5394.1960214247511077)
                * r
                + 687.1870074920579083)
                * r
                + 42.313330701600911252)
                * r
                + 1.0);
    }
    let tail = if q < 0.0 { p } else { 1.0 - p };
    let mut r = (-tail.ln()).sqrt();
    let value = if r <= 5.0 {
        r -= 1.6;
        (((((((r * 7.7454501427834140764e-4 + 0.0227238449892691845833) * r + 0.24178072517745061177) * r
            + 1.27045825245236838258)
            * r
            + 3.64784832476320460504)
            * r
            + 5.7694972214606914055)
            * r
            + 4.6303378461565452959)
            * r
            + 1.42343711074968357734)
            / (((((((r * 1.05075007164441684324e-9 + 5.475938084995344946e-4) * r + 0.0151986665636164571966)
                * r
                + 0.14810397642748007459)
                * r
                + 0.68976733498510000455)
                * r
                + 1.6763848301838038494)
                * r
                + 2.05319162663775882187)
                * r
                + 1.0)
    } else {
        r -= 5.0;
        (((((((r * 2.01033439929228813265e-7 + 2.71155556874348757815e-5) * r + 0.0012426609473880784386) * r
            + 0.026532189526576123093)
            * r
            + 0.29656057182850489123)
            * r
            + 1.7848265399172913358)
            * r
            + 5.4637849111641143699)
            * r
            + 6.6579046435011037772)
            / (((((((r * 2.04426310338993978564e-15 + 1.4215117583164458887e-7) * r + 1.8463183175100546818e-5)
                * r
                + 7.868691311456132591e-4)
                * r
                + 0.0148753612908506148525)
                * r
                + 0.13692988092273580531)
                * r
                + 0.59983220655588793769)
                * r
                + 1.0)
    };
    if q < 0.0 { -value } else { value }
}

struct Model {
    paths: usize,
    steps: usize,
    r0: f64,
    kappa: f64,
    theta: f64,
    sigma: f64,
    dt: f64,
    seed: u32,
}

fn vasicek(model: &Model) -> f64 {
    let mut generator = RGenerator::new(model.seed);
    let mut rates = vec![model.r0; model.paths];
    let mut total: f64 = rates.iter().sum();
    let volatility = model.sigma * model.dt.sqrt();
    for _ in 0..model.steps {
        for rate in rates.iter_mut() {
            let shock = generator.normal();
            *rate = *rate + model.kappa * (model.theta - *rate) * model.dt + volatility * shock;
        }
        total += rates.iter().sum::<f64>();
    }
    total
}

fn main() {
    let model = Model { paths: 200, steps: 2000, r0: 0.03, kappa: 0.5, theta: 0.04, sigma: 0.01, dt: 1.0 / 12.0, seed: 9 };
    measure("vasicek", || vasicek(black_box(&model)));
}

fn measure<T: PartialEq + Display>(name: &str, mut run: impl FnMut() -> T) {
    let started = Instant::now();
    let value = black_box(run());
    let cold = started.elapsed().as_secs_f64();
    let mut best = f64::INFINITY;
    for _ in 0..3 {
        let started = Instant::now();
        let again = black_box(run());
        best = best.min(started.elapsed().as_secs_f64());
        if again != value {
            eprintln!("the benchmark is not deterministic");
            std::process::exit(1);
        }
    }
    println!("{name} {value} {best:.4} {cold:.4}");
}
