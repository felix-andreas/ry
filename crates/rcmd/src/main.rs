//! `ry-rcmd`: what `bin/R` in ry's R home runs. `R CMD INSTALL`, `R CMD SHLIB`, `R CMD config`
//! and `R RHOME` are answered here; the R home is the directory `R_HOME` names.

use std::{path::PathBuf, process::ExitCode};

fn main() -> ExitCode {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let Some(home) = std::env::var_os("R_HOME").map(PathBuf::from) else {
        eprintln!("ry-rcmd: R_HOME is not set; run it through bin/R of an R home that `rcmd::assemble` created");
        return ExitCode::from(2);
    };
    match rcmd::run(&home, &arguments) {
        Ok(output) => {
            print!("{output}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}
