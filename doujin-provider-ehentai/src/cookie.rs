use std::error::Error;
use std::fmt;
use std::sync::{Arc, RwLock};

use reqwest::header::HeaderValue;

/// A validated Cookie request header. Its value is deliberately redacted from
/// `Debug`, `Display`, and parse errors.
#[derive(Clone, PartialEq, Eq)]
pub struct CookieHeader {
    value: HeaderValue,
    names: Vec<String>,
}

impl CookieHeader {
    pub fn parse(input: &str) -> Result<Self, CookieParseError> {
        if input.contains(['\r', '\n']) || !input.is_ascii() {
            return Err(CookieParseError::InvalidHeader);
        }

        let mut names = Vec::new();
        let mut normalized = Vec::new();
        for segment in input.split(';') {
            let segment = segment.trim();
            if segment.is_empty() {
                continue;
            }
            let (name, value) = segment
                .split_once('=')
                .ok_or(CookieParseError::InvalidPair)?;
            let name = name.trim();
            let value = value.trim();
            if name.is_empty()
                || !name.bytes().all(is_cookie_name_byte)
                || !value.bytes().all(is_cookie_value_byte)
            {
                return Err(CookieParseError::InvalidPair);
            }
            names.push(name.to_owned());
            normalized.push(format!("{name}={value}"));
        }
        if normalized.is_empty() {
            return Err(CookieParseError::Empty);
        }
        let value = HeaderValue::from_str(&normalized.join("; "))
            .map_err(|_| CookieParseError::InvalidHeader)?;
        Ok(Self { value, names })
    }

    pub fn cookie_names(&self) -> &[String] {
        &self.names
    }

    pub fn contains(&self, name: &str) -> bool {
        self.names.iter().any(|candidate| candidate == name)
    }

    /// Returns a header value solely for attaching it to an outbound request.
    /// Callers must not serialize or log the returned value.
    pub fn request_header_value(&self) -> HeaderValue {
        self.value.clone()
    }
}

fn is_cookie_name_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric()
        || matches!(
            byte,
            b'!' | b'#'
                | b'$'
                | b'%'
                | b'&'
                | b'\''
                | b'*'
                | b'+'
                | b'-'
                | b'.'
                | b'^'
                | b'_'
                | b'`'
                | b'|'
                | b'~'
        )
}

fn is_cookie_value_byte(byte: u8) -> bool {
    matches!(byte, b'!' | b'#'..=b'+' | b'-'..=b':' | b'<'..=b'[' | b']'..=b'~')
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CookieParseError {
    Empty,
    InvalidPair,
    InvalidHeader,
}

impl fmt::Display for CookieParseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Empty => "Cookie header 不可為空",
            Self::InvalidPair => "Cookie header 含有無效的 name=value pair",
            Self::InvalidHeader => "Cookie header 含有 HTTP header 不允許的字元",
        })
    }
}

impl Error for CookieParseError {}

impl fmt::Debug for CookieHeader {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CookieHeader")
            .field("value", &"[REDACTED]")
            .field("names", &self.names)
            .finish()
    }
}

impl fmt::Display for CookieHeader {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("[REDACTED]")
    }
}

/// Shared, dynamically updateable effective Cookie used by source and metadata
/// requests. A clone points to the same underlying state.
#[derive(Clone, Default)]
pub struct CookieStore {
    inner: Arc<RwLock<Option<CookieHeader>>>,
}

impl CookieStore {
    pub fn new(cookie: Option<CookieHeader>) -> Self {
        Self {
            inner: Arc::new(RwLock::new(cookie)),
        }
    }

    pub fn set(&self, cookie: Option<CookieHeader>) {
        match self.inner.write() {
            Ok(mut current) => *current = cookie,
            Err(poisoned) => *poisoned.into_inner() = cookie,
        }
    }

    pub fn snapshot(&self) -> Option<CookieHeader> {
        match self.inner.read() {
            Ok(current) => current.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        }
    }

    pub fn is_configured(&self) -> bool {
        self.snapshot().is_some()
    }
}

impl fmt::Debug for CookieStore {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CookieStore")
            .field("configured", &self.is_configured())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parser_preserves_extra_cookie_names_and_values_with_equals() {
        let cookie = CookieHeader::parse(
            "ipb_member_id=123; ipb_pass_hash=abc==; igneous=mystery; cf_clearance=extra",
        )
        .expect("valid cookie");
        assert_eq!(
            cookie.cookie_names(),
            &["ipb_member_id", "ipb_pass_hash", "igneous", "cf_clearance"]
        );
        assert!(cookie.contains("cf_clearance"));
    }

    #[test]
    fn parser_rejects_injection_and_malformed_pairs_without_echoing_input() {
        let secret = "ipb_pass_hash=do-not-leak\r\nX-Evil: yes";
        let error = CookieHeader::parse(secret).expect_err("header injection");
        assert!(!format!("{error:?} {error}").contains("do-not-leak"));
        assert!(CookieHeader::parse("valid=one; broken").is_err());
        assert!(CookieHeader::parse("bad name=value").is_err());
    }

    #[test]
    fn debug_display_and_shared_store_do_not_expose_cookie() {
        let cookie = CookieHeader::parse("ipb_pass_hash=do-not-leak").expect("valid cookie");
        assert!(!format!("{cookie:?} {cookie}").contains("do-not-leak"));
        let store = CookieStore::default();
        let clone = store.clone();
        store.set(Some(cookie));
        assert!(clone.snapshot().is_some());
        clone.set(None);
        assert!(!store.is_configured());
    }
}
