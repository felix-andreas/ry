//! The real crate links CoreFoundation, which zig cannot cross-link for macOS. chrono only
//! falls back to it after `TZ` and `/etc/localtime`, so answering "unknown" is safe. Stay
//! version- and feature-compatible with chrono's request, or cargo silently uses the real crate.

use std::error::Error;
use std::fmt;

#[derive(Debug)]
pub enum GetTimezoneError {
    FailedParsingString,
    IoError(std::io::Error),
    OsError,
}

impl fmt::Display for GetTimezoneError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            GetTimezoneError::FailedParsingString => write!(formatter, "GetTimezoneError::FailedParsingString"),
            GetTimezoneError::IoError(error) => write!(formatter, "GetTimezoneError::IoError({error})"),
            GetTimezoneError::OsError => write!(formatter, "GetTimezoneError::OsError"),
        }
    }
}

impl Error for GetTimezoneError {}

impl From<std::io::Error> for GetTimezoneError {
    fn from(error: std::io::Error) -> Self {
        GetTimezoneError::IoError(error)
    }
}

pub fn get_timezone() -> Result<String, GetTimezoneError> {
    Err(GetTimezoneError::OsError)
}
