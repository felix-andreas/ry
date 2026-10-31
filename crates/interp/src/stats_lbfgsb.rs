//! L-BFGS-B, the limited memory quasi-Newton method for bound constrained minimization behind
//! `optim(method = "L-BFGS-B")`.

use crate::{stats_optimize::Objective, unwind::{R, error}};

pub struct Settings {
    pub memory: usize,
    pub factor: f64,
    pub projected_tolerance: f64,
    pub max_iterations: i32,
    pub trace: i32,
    pub report: i32,
}

pub struct Outcome {
    pub solution: Vec<f64>,
    pub value: f64,
    pub function_count: usize,
    pub gradient_count: usize,
    pub fail: i32,
    pub message: String,
}

pub(crate) unsafe fn minimize(objective: &mut dyn Objective, start: &[f64], lower: &[f64], upper: &[f64], settings: &Settings) -> R<Outcome> {
    let _ = (objective, start, lower, upper, settings);
    Err(error("optim(method = \"L-BFGS-B\") is not implemented in ry yet"))
}
