//! The network services of ry's R runtime. Today that is the HTTP and HTTPS client (`http`),
//! which is kept out of the interpreter because its TLS and async stack is the largest
//! dependency an R session has and most code never touches it.
//!
//! The crate sits above the interpreter. A host calls [`install`] after `interp::install`.

pub mod http;

use interp::{extensions::Transport, interp};

/// Makes this crate's client the interpreter's HTTP transport. The interpreter must be
/// installed.
pub fn install() {
    interp().extensions.http = Transport {
        fetch: http::fetch,
        header_lines: http::header_lines,
        normalized: http::normalized,
        host: http::host,
    };
}
