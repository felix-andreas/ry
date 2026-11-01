//! HTTP and HTTPS transfers behind `url()`, `download.file(method = "libcurl")` and
//! `curlGetHeaders()`. GNU R uses libcurl; ry uses a blocking `reqwest` client with the
//! platform's root certificates and the standard proxy variables, and reports failures in
//! libcurl's words because R code and tests match on them.

use {
    reqwest::{
        StatusCode, Version,
        blocking::{Client, Response},
        redirect::Policy,
    },
    std::{sync::OnceLock, time::Duration},
};

/// A finished transfer: the final status, the content type the server declared, and the body.
pub struct Fetched {
    pub status: StatusCode,
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
    Status(StatusCode),
}

/// The status line GNU R quotes, such as `404 Not Found`.
pub fn status_text(status: StatusCode) -> String {
    match status.canonical_reason() {
        Some(reason) => format!("{} {reason}", status.as_u16()),
        None => status.as_u16().to_string(),
    }
}

/// The URL as libcurl prints it: parsed and normalized, so a bare host gains its trailing slash.
pub fn normalized(url: &str) -> String {
    reqwest::Url::parse(url).map_or_else(|_| url.to_owned(), |parsed| parsed.to_string())
}

/// GETs `url` with the extra request `headers` (each `"Name: value"`), following redirects.
pub fn fetch(url: &str, headers: &[String]) -> Result<Fetched, FetchError> {
    on_transfer_thread(|| fetch_here(url, headers))
}

fn fetch_here(url: &str, headers: &[String]) -> Result<Fetched, FetchError> {
    let response = send(client(true)?, url, headers, None)?;
    let status = response.status();
    if status.is_client_error() || status.is_server_error() {
        return Err(FetchError::Status(status));
    }
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let content_length = response.content_length();
    let body = response.bytes().map_err(transport)?.to_vec();
    Ok(Fetched {
        status,
        content_type,
        content_length,
        body,
    })
}

/// The response headers of `url` as libcurl reports them: for every hop, the status line, one
/// `Name: value` line per header and a blank line, each ending in CRLF, and the final status.
pub fn header_lines(
    url: &str,
    follow: bool,
    timeout: Option<Duration>,
) -> Result<(Vec<String>, u16), FetchError> {
    on_transfer_thread(|| header_lines_here(url, follow, timeout))
}

fn header_lines_here(
    url: &str,
    follow: bool,
    timeout: Option<Duration>,
) -> Result<(Vec<String>, u16), FetchError> {
    let mut lines = Vec::new();
    let mut current = url.to_owned();
    // libcurl's default limit on followed redirects in GNU R's configuration.
    for _ in 0..20 {
        let response = send(client(false)?, &current, &[], timeout)?;
        let status = response.status();
        let version = match response.version() {
            Version::HTTP_09 => "HTTP/0.9",
            Version::HTTP_10 => "HTTP/1.0",
            Version::HTTP_2 => "HTTP/2",
            Version::HTTP_3 => "HTTP/3",
            _ => "HTTP/1.1",
        };
        // HTTP/2 has no reason phrase, so libcurl prints the code followed by a space.
        let reason = if response.version() >= Version::HTTP_2 {
            ""
        } else {
            status.canonical_reason().unwrap_or("")
        };
        lines.push(format!("{version} {} {reason}\r\n", status.as_u16()));
        for (name, value) in response.headers() {
            lines.push(format!(
                "{name}: {}\r\n",
                String::from_utf8_lossy(value.as_bytes())
            ));
        }
        lines.push("\r\n".to_owned());
        let location = response
            .headers()
            .get(reqwest::header::LOCATION)
            .and_then(|value| value.to_str().ok())
            .and_then(|location| response.url().join(location).ok());
        match location {
            Some(next) if follow && status.is_redirection() => current = next.to_string(),
            _ => return Ok((lines, status.as_u16())),
        }
    }
    Err(FetchError::Transport {
        code: 47,
        message: "Number of redirects hit maximum amount",
        detail: "Maximum (20) redirects followed".to_owned(),
    })
}

/// Runs a whole transfer, from building the client to reading the body, on a thread of its own:
/// the blocking client runs an async runtime inside, which must not nest in one the embedding
/// program may already be running on the caller's thread.
fn on_transfer_thread<T: Send>(
    transfer: impl FnOnce() -> Result<T, FetchError> + Send,
) -> Result<T, FetchError> {
    std::thread::scope(|scope| scope.spawn(transfer).join()).unwrap_or_else(|_| {
        Err(FetchError::Transport {
            code: 2,
            message: "Failed initialization",
            detail: "the transfer thread stopped unexpectedly".to_owned(),
        })
    })
}

/// One client per redirect policy for the whole session, so connections are reused.
fn client(follow: bool) -> Result<&'static Client, FetchError> {
    static FOLLOWING: OnceLock<Result<Client, String>> = OnceLock::new();
    static SINGLE: OnceLock<Result<Client, String>> = OnceLock::new();
    let (cell, policy) = if follow {
        (&FOLLOWING, Policy::limited(20))
    } else {
        (&SINGLE, Policy::none())
    };
    let built = cell.get_or_init(|| {
        Client::builder()
            .redirect(policy)
            .user_agent(concat!("R (ry ", env!("CARGO_PKG_VERSION"), ")"))
            .build()
            .map_err(|failure| failure.to_string())
    });
    built.as_ref().map_err(|detail| FetchError::Transport {
        code: 2,
        message: "Failed initialization",
        detail: detail.clone(),
    })
}

fn send(
    client: &'static Client,
    url: &str,
    headers: &[String],
    timeout: Option<Duration>,
) -> Result<Response, FetchError> {
    let mut request = client.get(url);
    for header in headers {
        if let Some((name, value)) = header.split_once(':') {
            request = request.header(name.trim(), value.trim());
        }
    }
    if let Some(timeout) = timeout {
        request = request.timeout(timeout);
    }
    request.send().map_err(transport)
}

/// Maps a client error to the libcurl code and description GNU R would have reported.
fn transport(failure: reqwest::Error) -> FetchError {
    let mut detail = failure.to_string();
    let mut source = std::error::Error::source(&failure);
    while let Some(cause) = source {
        detail = cause.to_string();
        source = cause.source();
    }
    let lowered = detail.to_lowercase();
    let (code, message) = if failure.is_builder() {
        (3, "URL using bad/illegal format or missing URL")
    } else if failure.is_timeout() {
        (28, "Timeout was reached")
    } else if lowered.contains("dns")
        || lowered.contains("resolve")
        || lowered.contains("name or service not known")
    {
        (6, "Couldn't resolve host name")
    } else if lowered.contains("certificate") || lowered.contains("tls") {
        (60, "SSL peer certificate or SSH remote key was not OK")
    } else if failure.is_connect() {
        (7, "Couldn't connect to server")
    } else {
        (56, "Failure when receiving data from the peer")
    };
    FetchError::Transport {
        code,
        message,
        detail,
    }
}
