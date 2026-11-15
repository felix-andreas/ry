//! HTTP and HTTPS transfers behind `url()`, `download.file(method = "libcurl")` and
//! `curlGetHeaders()`, as the interpreter sees them. The client itself lives in the `services`
//! crate, which installs it as [`Transport`](crate::extensions::Transport); a host without it
//! fails every transfer the way libcurl fails a protocol it was built without. Failures are
//! reported in libcurl's words because R code and tests match on them.

use {crate::interp, std::time::Duration};

/// A finished transfer: the content type the server declared, the length it declared, and the
/// body.
pub struct Fetched {
    pub content_type: Option<String>,
    pub content_length: Option<u64>,
    pub body: Vec<u8>,
}

/// Why a transfer failed. `Transport` carries libcurl's error code and its short description,
/// `Status` a final HTTP status of 400 or above.
pub enum FetchError {
    Transport {
        code: u32,
        message: &'static str,
        detail: String,
    },
    Status(Status),
}

/// An HTTP status code with its standard reason phrase, when it has one.
#[derive(Clone, Copy)]
pub struct Status {
    pub code: u16,
    pub reason: Option<&'static str>,
}

impl Status {
    pub fn as_u16(self) -> u16 {
        self.code
    }
}

/// The status line GNU R quotes, such as `404 Not Found`.
pub fn status_text(status: Status) -> String {
    match status.reason {
        Some(reason) => format!("{} {reason}", status.code),
        None => status.code.to_string(),
    }
}

/// GETs `url` with the extra request `headers` (each `"Name: value"`), following redirects.
pub fn fetch(url: &str, headers: &[String]) -> Result<Fetched, FetchError> {
    (interp().extensions.http.fetch)(url, headers)
}

/// The response headers of `url` as libcurl reports them: for every hop, the status line, one
/// `Name: value` line per header and a blank line, each ending in CRLF, and the final status.
pub fn header_lines(
    url: &str,
    follow: bool,
    timeout: Option<Duration>,
) -> Result<(Vec<String>, u16), FetchError> {
    (interp().extensions.http.header_lines)(url, follow, timeout)
}

/// The URL as libcurl prints it: parsed and normalized, so a bare host gains its trailing slash.
pub fn normalized(url: &str) -> String {
    (interp().extensions.http.normalized)(url)
}

/// The host name in `url`, when it parses as a URL that has one.
pub fn host(url: &str) -> Option<String> {
    (interp().extensions.http.host)(url)
}
