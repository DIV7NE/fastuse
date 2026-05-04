//! Output cap helper — drops bytes once total accumulated stdout+stderr
//! exceeds [`super::OUTPUT_CAP_BYTES`]. Pipes still drain (we keep reading)
//! so the child doesn't block on its own pipe-full backpressure.

/// Tracks how much output has been accumulated; refuses further appends past
/// the cap. Caller still consumes the bytes off the pipe but discards them.
#[derive(Debug)]
pub struct OutputCap {
    cap: usize,
    used: usize,
    truncated: bool,
}

impl OutputCap {
    /// New cap with the given total-bytes ceiling.
    pub const fn new(cap: usize) -> Self {
        Self {
            cap,
            used: 0,
            truncated: false,
        }
    }

    /// Try to append `n` bytes; returns the number actually accepted. If the
    /// return is less than `n`, the cap fired and `truncated()` is now true.
    pub fn admit(&mut self, n: usize) -> usize {
        if self.used >= self.cap {
            self.truncated = true;
            return 0;
        }
        let remaining = self.cap - self.used;
        let take = remaining.min(n);
        self.used += take;
        if take < n {
            self.truncated = true;
        }
        take
    }

    /// True if any bytes were dropped.
    pub const fn truncated(&self) -> bool {
        self.truncated
    }

    /// Bytes accepted so far.
    pub const fn used(&self) -> usize {
        self.used
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn admits_under_cap() {
        let mut c = OutputCap::new(100);
        assert_eq!(c.admit(50), 50);
        assert_eq!(c.admit(40), 40);
        assert!(!c.truncated());
        assert_eq!(c.used(), 90);
    }

    #[test]
    fn caps_at_boundary() {
        let mut c = OutputCap::new(100);
        assert_eq!(c.admit(60), 60);
        assert_eq!(c.admit(50), 40); // only 40 left
        assert!(c.truncated());
        assert_eq!(c.used(), 100);
    }

    #[test]
    fn admits_zero_after_cap() {
        let mut c = OutputCap::new(10);
        c.admit(20);
        assert!(c.truncated());
        assert_eq!(c.admit(5), 0);
    }
}
