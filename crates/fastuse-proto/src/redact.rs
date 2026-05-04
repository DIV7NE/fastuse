//! `Redact<T>` newtype: payload-safe Display/Debug.
//!
//! Wraps any `T: AsRef<[u8]>` (or string-like) and prints `<redacted N bytes>`
//! instead of the payload contents. Per D-10, every clipboard / typed-text /
//! image-bytes value at a tool boundary MUST use `Redact<T>`. The `xtask
//! check-redact` lint enforces this on `fastuse-proto` request/response variants.

use core::fmt;
use serde::{Deserialize, Serialize};

/// Newtype that hides its payload from `Display` and `Debug`.
///
/// `Display` and `Debug` both render `<redacted N bytes>` where `N` is the
/// length of the inner value's byte representation. The inner value is only
/// recoverable via `into_inner()` or `as_ref()` — both explicit accessors that
/// audit-grep can flag.
#[derive(Clone, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(transparent)]
pub struct Redact<T>(T);

impl<T> Redact<T> {
    /// Wrap a value so that `Display`/`Debug` never expose its bytes.
    pub const fn new(inner: T) -> Self {
        Self(inner)
    }

    /// Consume the wrapper and yield the inner value (audit-greppable).
    pub fn into_inner(self) -> T {
        self.0
    }

    /// Borrow the inner value (audit-greppable).
    pub fn as_inner(&self) -> &T {
        &self.0
    }
}

impl<T: RedactLen> fmt::Display for Redact<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "<redacted {} bytes>", self.0.redact_len())
    }
}

impl<T: RedactLen> fmt::Debug for Redact<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Identical to Display — never expose payload via {:?}.
        write!(f, "<redacted {} bytes>", self.0.redact_len())
    }
}

/// Length helper. Implemented for the small set of payload types we wrap.
pub trait RedactLen {
    /// Number of bytes the payload represents (used in the redacted display).
    fn redact_len(&self) -> usize;
}

impl RedactLen for Vec<u8> {
    fn redact_len(&self) -> usize {
        self.len()
    }
}

impl RedactLen for String {
    fn redact_len(&self) -> usize {
        self.len()
    }
}

impl<const N: usize> RedactLen for [u8; N] {
    fn redact_len(&self) -> usize {
        N
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redact_default_is_zero_bytes() {
        let r: Redact<Vec<u8>> = Redact::default();
        assert_eq!(format!("{r}"), "<redacted 0 bytes>");
    }

    #[test]
    fn redact_display_hides_payload() {
        let payload: Vec<u8> = std::iter::repeat([0xDE_u8, 0xAD, 0xBE, 0xEF])
            .take(25)
            .flatten()
            .collect(); // 100 bytes
        let r = Redact::new(payload);
        let s = format!("{r}");
        assert_eq!(s, "<redacted 100 bytes>");
        assert!(!s.contains("DE"));
        assert!(!s.contains("AD"));
        assert!(!s.contains("BE"));
        assert!(!s.contains("EF"));
    }

    #[test]
    fn redact_debug_equals_display() {
        let r = Redact::new(b"secret-data".to_vec());
        let d = format!("{r:?}");
        let s = format!("{r}");
        assert_eq!(d, s);
        assert!(!d.contains("secret"));
    }

    #[test]
    fn redact_string_hides_text() {
        let r = Redact::new(String::from("password123"));
        let s = format!("{r}");
        assert_eq!(s, "<redacted 11 bytes>");
        assert!(!s.contains("password"));
    }

    #[test]
    fn redact_arbitrary_lengths() {
        for n in [0_usize, 1, 7, 64, 1024, 65536] {
            let r = Redact::new(vec![0u8; n]);
            assert_eq!(format!("{r}"), format!("<redacted {n} bytes>"));
        }
    }
}
