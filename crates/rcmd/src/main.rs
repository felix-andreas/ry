//! `ry-rcmd`: ry's R front end on its own (`rcmd::front_end`).

use std::process::ExitCode;

fn main() -> ExitCode {
    rcmd::front_end::main(std::env::args().skip(1).collect())
}
