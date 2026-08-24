pub trait Clock: Send + Sync {
    fn now(&self) -> std::time::SystemTime;
}

pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> std::time::SystemTime {
        std::time::SystemTime::now()
    }
}

// Gated on `test` (this crate's own tests) OR the `testing` feature, not bare
// `#[cfg(test)]` alone: `#[cfg(test)]` items are only visible inside this
// crate's own test compilation and are never exported to a downstream crate,
// even in that crate's tests. Unit 09 (lsbx-lifecycle)'s lease-expiry tests
// need to construct a `FakeClock` from outside this crate, so the `testing`
// feature is the escape hatch that makes that possible.
#[cfg(any(test, feature = "testing"))]
pub struct FakeClock {
    pub now: std::time::SystemTime,
}

#[cfg(any(test, feature = "testing"))]
impl Clock for FakeClock {
    fn now(&self) -> std::time::SystemTime {
        self.now
    }
}
